import AmuxValues
import AmuxWords
import SwiftUI

/// Every agent the profile reaches, in the runtime's order, with the
/// settled ones behind one row at the foot.
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

    private var fleet: FleetContent { content.fleet }

    var body: some View {
        VStack(spacing: 0) {
            if place.settled {
                settledHeader
            } else {
                VStack(spacing: 1) {
                    SidebarAction(symbol: "square-pen", title: "New Agent", shortcut: "⌘N") {
                        actions.newAgent()
                    }
                    SearchField(text: $search, focused: $searching)
                        .onChange(of: search) { _, words in actions.search(words) }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 4)
                if content.toFinish > 0 {
                    FinishSettingUp(count: content.toFinish, actions: actions)
                }
            }
            list
            if !place.settled, let settled = fleet.settled {
                SettledFoot(count: Int(settled.families)) { place.settled = true }
            }
        }
        .background {
            // ⌘F reaches the search from anywhere in the window.
            Button("Search") { searching = true }
                .keyboardShortcut("f")
                .opacity(0)
                .frame(width: 0, height: 0)
                .accessibilityHidden(true)
        }
        .alert("Rename Agent", isPresented: isRenaming, presenting: renaming) { row in
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

    private var rows: [FleetRow] {
        place.settled ? fleet.settled?.rows ?? [] : fleet.live.flatMap(\.rows)
    }

    @ViewBuilder private var list: some View {
        if rows.isEmpty {
            empty.frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollViewReader { reader in
                ScrollView {
                    // One flat list, so a row keeps its identity when it
                    // moves between sections and redraws in its new place.
                    LazyVStack(alignment: .leading, spacing: 1) {
                        ForEach(entries) { entry in
                            switch entry {
                            case .title(_, let text): SectionTitle(text: text)
                            case .row(let row): item(row)
                            }
                        }
                    }
                    .padding(.horizontal, 8)
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
            (title(section).map { [Entry.title(section.kind, $0)] } ?? []) + section.rows.map(Entry.row)
        }
    }

    private func item(_ row: FleetRow) -> some View {
        FleetRowView(
            row: row, now: fleet.now, selected: place.agent == row.card.agent,
            local: row.card.agent.host == content.thisMac?.hostId,
            toggleFamily: { actions.toggleFamily(row.card.agent) }, quick: quick(row)
        )
        .onTapGesture {
            place.agent = row.card.agent
            listFocused = true
        }
        .contextMenu { menu(row) }
    }

    /// A heading over a section, where there is more than one kind.
    private func title(_ section: FleetSection) -> String? {
        guard fleet.live.count > 1 else { return nil }
        return switch section.kind {
        case .pinned: "Pinned"
        case .needsYou: "Needs you"
        case .live: section.kind == fleet.live.first?.kind ? nil : "Recent"
        case .closed: nil
        }
    }

    /// Moves the selection up or down the list, as arrow keys do in a
    /// sidebar.
    private func step(_ by: Int, _ reader: ScrollViewProxy) -> KeyPress.Result {
        let keys = rows.map(\.card.agent)
        guard !keys.isEmpty else { return .ignored }
        let at = place.agent.flatMap { keys.firstIndex(of: $0) }
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
            Button("Don’t Settle") { actions.act(.cancelClose, agent) }
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

    private var settledHeader: some View {
        Button {
            place.settled = false
        } label: {
            HStack(spacing: 8) {
                Icon("chevron-left", size: 14)
                    .foregroundStyle(.secondary)
                    .frame(width: 18)
                Text("Settled").font(.ui(.body, weight: .semibold))
                Spacer()
            }
            .padding(.horizontal, 8)
            .frame(height: 28)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
        .padding(.horizontal, 8)
        .padding(.bottom, 4)
        .keyboardShortcut(.escape, modifiers: [])
    }

    @ViewBuilder private var empty: some View {
        if !fleet.loaded {
            ProgressView().controlSize(.small)
        } else if place.settled {
            Text("Nothing settled").foregroundStyle(.secondary)
        } else if !search.isEmpty {
            Text("No agents say “\(search)”").foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
        } else {
            Text("No agents yet").foregroundStyle(.tertiary)
        }
    }
}

/// Something a row offers under the pointer.
struct QuickAction {
    let symbol: String
    let help: String
    let action: () -> Void
}

/// One agent as a card of two lines beside its repository's tile: its name
/// and age, then where it works ("amux · build-box"). What it is doing is a
/// badge on the tile; when it needs the person, or failed, the second line
/// says why instead. Every row is the same height, so the list keeps one
/// rhythm.
struct FleetRowView: View {
    let row: FleetRow
    let now: Date
    let selected: Bool
    /// It runs on this Mac, which its second line calls so.
    var local = false
    /// The surface the row sits on, which rings its tile's badge.
    var ground: Color = Palette.sidebar
    let toggleFamily: () -> Void
    var quick: [QuickAction] = []

    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            if row.depth == 0 {
                RepositoryTile(repository: row.card.repository, size: 30, quiet: dimmed)
                    .overlay(alignment: .bottomTrailing) {
                        StatusBadge(
                            state: state, ground: ground,
                            lit: selected ? Palette.selection : (hovering ? Palette.hover : .clear)
                        )
                        .offset(x: 4, y: 4)
                    }
            } else {
                // A member works where its family does, which the head's
                // tile says: it keeps only its state, on the family's line.
                ZStack {
                    Rectangle().fill(Palette.line).frame(width: 1.5)
                    StatusBadge(state: state, ground: ground, lit: selected ? Palette.selection : (hovering ? Palette.hover : .clear))
                }
                .frame(width: 30, height: 48)
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(row.name)
                        .font(.ui(.body, weight: .medium))
                        .foregroundStyle(dimmed ? .secondary : .primary)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    trailing
                }
                .frame(height: 17)
                HStack(spacing: 4) {
                    Text(second.text)
                        .font(.ui(.meta))
                        .foregroundStyle(second.color)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    disclosure
                }
                .frame(height: 15)
            }
        }
        // A member's line runs under its head's tile, and its words line up
        // with the head's.
        .padding(.leading, 8 + CGFloat(max(0, Int(row.depth) - 1)) * 40)
        .padding(.trailing, 8)
        .frame(height: 48)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(selected ? Palette.selection : (hovering ? Palette.hover : .clear)))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .help(described)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
    }

    private var state: RowState { RowState(row: row) }

    /// Settled and finished agents read quieter than the ones at work.
    private var dimmed: Bool {
        switch state {
        case .settled, .exited(.ended), .exited(.finished): true
        default: false
        }
    }

    /// Under the pointer, the row's own actions; else its age.
    @ViewBuilder private var trailing: some View {
        if hovering, !quick.isEmpty {
            HStack(spacing: 0) {
                ForEach(Array(quick.enumerated()), id: \.offset) { _, action in
                    Button(action: action.action) {
                        Icon(action.symbol, size: 13)
                            .foregroundStyle(.secondary)
                            .frame(width: 22, height: 18)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(HoverButtonStyle())
                    .help(action.help)
                }
            }
        } else {
            Text(row.age(at: now))
                .font(.ui(.small))
                .foregroundStyle(.tertiary)
                .monospacedDigit()
        }
    }

    /// A family's size and whether its members are listed, at the end of
    /// the head's second line.
    @ViewBuilder private var disclosure: some View {
        if row.children > 0 {
            Button(action: toggleFamily) {
                HStack(spacing: 2) {
                    Text("\(row.children)").monospacedDigit()
                    Icon(row.expanded ? "chevron-down" : "chevron-right", size: 10)
                }
                .font(.ui(.caption))
                .foregroundStyle(.tertiary)
                .padding(.horizontal, 4)
                .frame(height: 15)
                .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .help(row.expanded ? "Fold the family" : "List the family")
        }
    }

    /// The second line: what the agent asks, or why it stopped; else where
    /// it works.
    private var second: (text: String, color: Color) {
        switch state {
        case .needsYou, .exited(.failed):
            let said = said
            return (said.text, said.color == Palette.you ? Palette.you : .secondary)
        default:
            if let loud = row.loud, let line = FleetWords.secondLine(loud.secondLine, kind: row.card.kind, now: now),
                line.ink == .ask
            {
                return ("\(loud.name): \(line.text)", Palette.you)
            }
            if row.depth > 0 {
                // A member's place is its family's: it says what it is doing.
                let said = said
                return (said.text, said.color == Palette.you ? Palette.you : .secondary)
            }
            return (place, .secondary)
        }
    }

    /// "amux · build-box": the repository, then the host.
    private var place: String {
        let host = local ? "This Mac" : PlaceNames.host(row.hostName)
        guard let name = row.card.repository?.name, !name.isEmpty else { return host }
        return "\(name) · \(host)"
    }

    /// What the row would say on its second line, as every client words it.
    private var said: (text: String, color: Color) {
        if let loud = row.loud,
            let line = FleetWords.secondLine(loud.secondLine, kind: row.card.kind, now: now)
        {
            return ("\(loud.name): \(line.text)", color(line.ink))
        }
        if let line = FleetWords.secondLine(
            row.secondLine, kind: row.card.kind, now: now, backgroundJobs: row.card.backgroundJobs)
        {
            return (line.text, color(line.ink))
        }
        let word = state.word ?? ""
        let host = PlaceNames.host(row.hostName)
        return (state.namesTheHost ? word : "\(word) · \(host)", .secondary)
    }

    /// The row in full, for the tooltip: its state, host and folder.
    private var described: String {
        [said.text, "\(PlaceNames.host(row.hostName)) · \(PlaceNames.path(row.workingDirectory))"]
            .filter { !$0.isEmpty }.joined(separator: "\n")
    }

    private func color(_ ink: FleetWords.Ink) -> Color {
        switch ink {
        case .ask: Palette.you
        case .quiet: .secondary
        case .warning: Palette.caution
        case .error: Palette.failure
        }
    }
}

/// "Settled 4 ›" at the sidebar's foot.
private struct SettledFoot: View {
    let count: Int
    let open: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Rectangle().fill(Palette.line).frame(height: 1)
            Button(action: open) {
                HStack {
                    Text("Settled")
                    Spacer()
                    Text("\(count)").foregroundStyle(.tertiary).monospacedDigit()
                    Icon("chevron-right", size: 12)
                        .foregroundStyle(.tertiary)
                }
                .font(.ui(.body))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 8)
                .frame(height: 30)
                .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
        }
    }
}

/// Search as a plain row, the field showing only once it is used, as the
/// references' is.
struct SearchField: View {
    @Binding var text: String
    var prompt = "Search"
    var focused: FocusState<Bool>.Binding?
    @FocusState private var own: Bool
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 8) {
            Icon("search", size: 15)
                .foregroundStyle(.secondary)
                .frame(width: 18)
            field
                .textFieldStyle(.plain)
                .font(.ui(.body))
                .onExitCommand { text = "" }
            if !text.isEmpty {
                Button {
                    text = ""
                } label: {
                    Icon("x", size: 12).foregroundStyle(.tertiary)
                }
                .buttonStyle(.plain)
                .help("Clear")
            } else if !isFocused {
                Text("⌘F").font(.ui(.small)).foregroundStyle(.tertiary)
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

/// "Finish Setting Up (n)", until those steps are done or it is dismissed.
private struct FinishSettingUp: View {
    let count: UInt32
    let actions: MainActions

    var body: some View {
        HStack(spacing: 8) {
            Icon("list-checks", size: 14).foregroundStyle(.tint)
            Button("Finish Setting Up (\(count))") { actions.finishSettingUp() }
                .buttonStyle(.link)
            Spacer()
            Button {
                actions.dismissFinishing()
            } label: {
                Icon("x", size: 12)
            }
            .buttonStyle(.borderless)
            .help("Dismiss")
        }
        .font(.ui(.meta))
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.well))
        .padding(.horizontal, 10)
        .padding(.bottom, 6)
    }
}
