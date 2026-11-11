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

    private var fleet: FleetContent { content.fleet }

    var body: some View {
        VStack(spacing: 0) {
            if place.settled {
                settledHeader
            } else {
                SearchField(text: $search)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 6)
                    .onChange(of: search) { _, words in actions.search(words) }
                if content.toFinish > 0 {
                    FinishSettingUp(count: content.toFinish, actions: actions)
                }
            }
            list
            if !place.settled, let settled = fleet.settled {
                SettledFoot(count: Int(settled.families)) { place.settled = true }
            }
        }
        .alert("Rename Agent", isPresented: isRenaming, presenting: renaming) { row in
            TextField("Name", text: $newName)
            Button("Rename") { actions.act(.rename(newName), row.card.agent) }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog(
            deleting.map { "Delete \($0.name)?" } ?? "", isPresented: isDeleting, presenting: deleting
        ) { row in
            Button("Delete", role: .destructive) { actions.act(.delete, row.card.agent) }
        } message: { _ in
            Text("Its chat is deleted on its host too. This cannot be undone.")
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
            toggleFamily: { actions.toggleFamily(row.card.agent) }
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
        case .live: "Agents"
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
        Divider()
        Button("Delete…", role: .destructive) { deleting = row }
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
            HStack(spacing: 6) {
                Image(systemName: "chevron.left").font(.system(size: 11, weight: .semibold))
                Text("Settled").font(.system(size: 13, weight: .semibold))
                Spacer()
            }
            .foregroundStyle(.secondary)
            .padding(.horizontal, 10)
            .frame(height: 28)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
        .padding(.horizontal, 8)
        .padding(.bottom, 6)
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
            VStack(spacing: 10) {
                Text("No agents yet").foregroundStyle(.secondary)
                Button("New Agent") { actions.newAgent() }
            }
        }
    }
}

/// One agent: its name and age, then what it is doing or wants.
struct FleetRowView: View {
    let row: FleetRow
    let now: Date
    let selected: Bool
    let toggleFamily: () -> Void

    @State private var hovering = false

    var body: some View {
        HStack(alignment: .top, spacing: 4) {
            disclosure
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(row.name)
                        .font(.system(size: 13, weight: needsYou ? .semibold : .regular))
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    if needsYou {
                        Circle().fill(Color.accentColor).frame(width: 7, height: 7)
                    }
                    Text(row.age(at: now))
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                        .monospacedDigit()
                }
                Text(second.text)
                    .font(.system(size: 12))
                    .foregroundStyle(second.color)
                    .lineLimit(1)
            }
        }
        .padding(.leading, 6 + CGFloat(row.depth) * 14)
        .padding(.trailing, 10)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(selected ? Palette.selection : (hovering ? Palette.hover : .clear)))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
    }

    private var needsYou: Bool { row.card.familyAttention == .needsYou }

    /// A chevron for a family's members, folded or listed; the same room
    /// left blank on a row without any, so names line up.
    private var disclosure: some View {
        Group {
            if row.children > 0 {
                Button(action: toggleFamily) {
                    Image(systemName: row.expanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(.tertiary)
                        .frame(width: 12, height: 17)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(row.expanded ? "Fold the family" : "List the family")
            } else {
                Color.clear.frame(width: 12, height: 17)
            }
        }
    }

    /// The second line: what the agent asks or is doing, as every client
    /// words it, else its state and host.
    private var second: (text: String, color: Color) {
        let state = RowState(row: row)
        if let loud = row.loud,
            let said = FleetWords.secondLine(loud.secondLine, kind: row.card.kind, now: now)
        {
            return ("\(loud.name): \(said.text)", color(said.ink))
        }
        if let said = FleetWords.secondLine(
            row.secondLine, kind: row.card.kind, now: now, backgroundJobs: row.card.backgroundJobs)
        {
            return (said.text, color(said.ink))
        }
        let word = state.word ?? ""
        let host = PlaceNames.host(row.hostName)
        return (state.namesTheHost ? word : "\(word) · \(host)", .secondary)
    }

    private func color(_ ink: FleetWords.Ink) -> Color {
        switch ink {
        case .ask: .accentColor
        case .quiet: .secondary
        case .warning: .orange
        case .error: .red
        }
    }
}

struct SectionTitle: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 11, weight: .medium))
            .foregroundStyle(.tertiary)
            .padding(.leading, 10)
            .padding(.top, 12)
            .padding(.bottom, 4)
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
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.tertiary)
                }
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 10)
                .frame(height: 30)
                .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle())
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
        }
    }
}

struct SearchField: View {
    @Binding var text: String
    var prompt = "Search"

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 12))
                .foregroundStyle(.tertiary)
            TextField(prompt, text: $text)
                .textFieldStyle(.plain)
                .font(.system(size: 13))
            if !text.isEmpty {
                Button {
                    text = ""
                } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary)
                }
                .buttonStyle(.plain)
                .help("Clear")
            }
        }
        .padding(.horizontal, 8)
        .frame(height: 28)
        .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(Palette.hover))
    }
}

/// "Finish Setting Up (n)", until those steps are done or it is dismissed.
private struct FinishSettingUp: View {
    let count: UInt32
    let actions: MainActions

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "checklist").foregroundStyle(.tint)
            Button("Finish Setting Up (\(count))") { actions.finishSettingUp() }
                .buttonStyle(.link)
            Spacer()
            Button {
                actions.dismissFinishing()
            } label: {
                Image(systemName: "xmark").font(.system(size: 10, weight: .semibold))
            }
            .buttonStyle(.borderless)
            .help("Dismiss")
        }
        .font(.system(size: 12))
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Palette.hover))
        .padding(.horizontal, 10)
        .padding(.bottom, 6)
    }
}
