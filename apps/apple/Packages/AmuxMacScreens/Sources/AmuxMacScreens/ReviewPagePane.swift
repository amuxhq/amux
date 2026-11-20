import AmuxValues
import AmuxWords
import SwiftUI

/// The tool window's Review tab over the agent's review, read and written
/// at the agent's host, drawn as the runtime composed it: the view shown
/// and the switch to the other in the bar, what the page says above the
/// files, then each file under a sticky head with its hunks, the lines
/// between them that more context loads, and the comments where they sit.
///
/// A click on a line picks it and opens a comment on it; dragging across
/// lines, or their numbers, picks them all, within the file it began in,
/// with the list running under a pointer held at its top or bottom edge;
/// Shift-click takes in more. From the keyboard, the arrow keys move a line at a time and
/// Shift with them takes in more, Return opens a comment on what is
/// picked, and Escape steps back out. Every draft written here rides on
/// the chat's next message under the composer's chip.
struct ReviewPagePane: View {
    let review: any ReviewPageSurface
    /// The row the arrow keys move from, by key, so a page that moves
    /// under the reader keeps it.
    @State private var head: String?
    /// Where the lines are and where a drag picking them is; read on every
    /// movement and scrolled frame, so it is not observed.
    @State private var drawing = Drawing()
    /// Moved while a drag runs the list under a pointer at an edge.
    @State private var position = ScrollPosition()
    @State private var discarding = false
    @State private var startingOver = false
    @FocusState private var focused: Bool

    var body: some View {
        let page = review.page
        VStack(spacing: 0) {
            WindowBar(leading: 8) { bar(page) }
            if page.header != nil {
                ForEach(Array(page.banners.enumerated()), id: \.offset) { _, banner in
                    BannerRow(banner: banner, act: { review.refresh() })
                }
            }
            content(page).frame(maxWidth: .infinity, maxHeight: .infinity)
            if let notice = review.notice {
                Text(notice)
                    .font(.ui(.meta))
                    .foregroundStyle(Palette.failure)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .overlay(alignment: .top) { Rectangle().fill(Palette.line).frame(height: 1) }
            }
        }
        .confirmationDialog(
            ChatWords.discardDrafts(page.header?.drafts ?? 0), isPresented: $discarding
        ) {
            Button("Discard Drafts", role: .destructive) { review.discardDrafts() }
        } message: {
            Text(ChatWords.discardDraftsDetail)
        }
        .confirmationDialog(ChatWords.startOverQuestion, isPresented: $startingOver) {
            Button("Start Over", role: .destructive) {
                head = nil
                review.startOver()
            }
        } message: {
            Text(ChatWords.startOverDetail)
        }
    }

    // MARK: - The bar

    @ViewBuilder private func bar(_ page: ReviewPage) -> some View {
        if let header = page.header {
            ViewSwitch(header: header, show: { review.show($0) })
            Spacer(minLength: 6)
            if review.waiting > 0 { ProgressView().controlSize(.mini) }
            if header.files > 0 { counts(header) }
            menu(header)
        } else if page.opening {
            // The tab above names the tool; nothing is open to say more.
            Spacer()
        } else {
            // Nothing could be opened: why, in a word, beside the way to
            // ask again. The note below says it in full.
            Text(Self.unopened(page.banners))
                .font(.ui(.meta))
                .foregroundStyle(Palette.textSecondary)
                .lineLimit(1)
            Spacer(minLength: 6)
            if review.waiting > 0 { ProgressView().controlSize(.mini) }
            Button("Try Again") { review.refresh() }
                .buttonStyle(HoverButtonStyle())
                .font(.ui(.meta))
                .fixedSize()
                .help("Open the review again")
        }
    }

    /// Why no review opened, as the bar says it.
    static func unopened(_ banners: [ReviewBanner]) -> String {
        switch banners.first {
        case .notOffered?: "Reviews are off on this computer"
        case .notARepository?: "Not a git repository"
        case .unreachable?: "The computer can’t be reached"
        case .snapshotFailed?: "No snapshot of the changes"
        default: "No review"
        }
    }

    private func counts(_ header: ReviewHeader) -> some View {
        HStack(spacing: 6) {
            Text(ChatWords.files(Int(header.files))).foregroundStyle(Palette.textSecondary)
            if header.added > 0 { Text("+\(header.added)").foregroundStyle(Palette.added) }
            if header.removed > 0 { Text("−\(header.removed)").foregroundStyle(Palette.removed) }
        }
        .font(.ui(.meta))
        .monospacedDigit()
        .lineLimit(1)
        .fixedSize()
        .help(ChatWords.reviewCounts(header))
    }

    private func menu(_ header: ReviewHeader) -> some View {
        Menu {
            Button("Refresh") { review.refresh() }
            Divider()
            Button("Discard Drafts…") { discarding = true }.disabled(!header.discardDrafts)
            Button("Start Over…") { startingOver = true }.disabled(!header.startOver)
        } label: {
            Icon("ellipsis", size: 15, optical: false)
                .foregroundStyle(Palette.textSecondary)
                .frame(width: 28, height: 26)
                .contentShape(Rectangle())
        }
        .menuStyle(.button)
        .buttonStyle(HoverButtonStyle())
        .menuIndicator(.hidden)
        .fixedSize()
        .help("More")
    }

    // MARK: - The page

    @ViewBuilder private func content(_ page: ReviewPage) -> some View {
        if page.header == nil {
            if page.opening {
                VStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text(ChatWords.reviewOpening).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
                }
            } else {
                // Nothing could be opened: what the page says is all of it.
                VStack(spacing: 14) {
                    ForEach(Array(page.banners.enumerated()), id: \.offset) { _, banner in
                        CentreNote(
                            symbol: BannerRow.symbol(banner), text: ChatWords.reviewBanner(banner),
                            action: ChatWords.reviewBannerAction(banner), act: { review.refresh() })
                    }
                }
                .padding(24)
            }
        } else if page.rows.isEmpty, let empty = page.empty {
            CentreNote(
                symbol: empty == .nothing ? "circle-check" : "diff", text: ChatWords.reviewEmpty(empty),
                action: empty == .nothingSince ? ChatWords.reviewView(.all) : nil, act: { review.show(.all) })
                .padding(24)
        } else {
            rows(page)
        }
    }

    private func rows(_ page: ReviewPage) -> some View {
        let sections = FileSection.of(page.rows)
        return ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    if page.empty == .nothingSince {
                        // Since your last review holds no change, only the
                        // comments left from before.
                        NothingSince { review.show(.all) }
                    }
                    ForEach(sections) { section in
                        Section {
                            if let at = section.head, writer(after: at) { writerRow(page) }
                            ForEach(section.rows) { item in
                                row(item, page.rows[item.index])
                                if writer(after: item.index) { writerRow(page) }
                            }
                        } header: {
                            if let at = section.head, case .file(let file) = page.rows[at].of {
                                FileHeadRow(
                                    file: file, view: page.header?.view, rounds: page.header?.rounds ?? 0,
                                    selected: isSelected(at),
                                    toggle: { review.toggleFile(file.path) },
                                    commentOnFile: {
                                        review.select(at)
                                        head = page.rows[at].key
                                        review.writing = true
                                    })
                                .id(page.rows[at].key)
                            }
                        }
                    }
                }
                .padding(.bottom, 12)
                .coordinateSpace(.named(Self.space))
                .simultaneousGesture(
                    DragGesture(minimumDistance: 0, coordinateSpace: .named(Self.space))
                        .onChanged(drag)
                        .onEnded { _ in finishDrag() })
            }
            .scrollPosition($position)
            .onScrollGeometryChange(for: ScrollGeometry.self, of: { $0 }) { _, geometry in
                drawing.scroll = geometry
            }
            .focusable()
            .focused($focused)
            .focusEffectDisabled()
            .onKeyPress(keys: [.upArrow, .downArrow], phases: [.down, .repeat]) { press in
                move(by: press.key == .upArrow ? -1 : 1, extending: press.modifiers.contains(.shift))
                if let head { proxy.scrollTo(head) }
                return .handled
            }
            .onKeyPress(.return) {
                guard review.anchor != nil else { return .ignored }
                review.writing = true
                return .handled
            }
            .onKeyPress(.escape) {
                guard review.selected != nil else { return .ignored }
                review.clearSelection()
                return .handled
            }
            .onChange(of: review.writing) { _, writing in
                // Back to the lines when the field closes, so the keys
                // keep moving the selection.
                if !writing { focused = true }
            }
        }
    }

    @ViewBuilder private func row(_ item: FileSection.Item, _ row: ReviewRow) -> some View {
        switch row.of {
        case .file:
            EmptyView()
        case .comment(let comment):
            CommentCard(
                comment: comment, toggle: { review.toggleComment(comment.id) },
                reword: { review.reword(comment.id, as: $0) }, delete: { review.delete(comment.id) })
        case .hunk(let hunk):
            HunkHeadRow(hunk: hunk)
        case .line(let line):
            CodeLineRow(line: line, selected: isSelected(item.index))
                .id(row.key)
                .onGeometryChange(for: CGRect.self) { $0.frame(in: .named(Self.space)) } action: { frame in
                    drawing.frames[row.key] = frame
                }
        case .gap(let gap):
            GapRow(gap: gap, more: { review.moreContext($0) })
        case .loading:
            HStack(spacing: 8) {
                ProgressView().controlSize(.mini)
                Text("Loading…")
            }
            .font(.ui(.meta))
            .foregroundStyle(Palette.textTertiary)
            .padding(.horizontal, 12)
            .frame(height: 30)
        case .noLines(_, let why):
            Text(ChatWords.reviewNoLines(why))
                .font(.ui(.meta))
                .foregroundStyle(Palette.textTertiary)
                .padding(.horizontal, 12)
                .frame(height: 30)
        }
    }

    // MARK: - Selecting

    private func isSelected(_ row: Int) -> Bool {
        review.selected?.contains(row) ?? false
    }

    /// The lines' coordinate space: the list's content, scrolled with it.
    private static let space = "review.lines"

    /// The pointer pressed or moved, from where it went down: a press on a
    /// line picks it, or with Shift takes in the lines up to it; moving
    /// then stretches the pick through the lines of that file.
    private func drag(_ value: DragGesture.Value) {
        let rows = review.page.rows
        if drawing.file == nil {
            guard !drawing.missed else { return }
            drawing.places = Dictionary(
                rows.enumerated().map { ($1.key, $0) }, uniquingKeysWith: { first, _ in first })
            guard let index = drawing.line(at: value.startLocation.y, within: nil) else {
                // Pressed on something else: a head, a gap, a comment.
                drawing.missed = true
                return
            }
            drawing.file = FileSection.range(of: index, in: rows)
            drawing.reached = index
            if NSEvent.modifierFlags.contains(.shift), review.selected != nil {
                review.extend(to: index)
            } else {
                review.select(index)
            }
            head = rows[index].key
        }
        reach(value.location.y)
        drawing.pointer = value.location.y - (drawing.scroll?.contentOffset.y ?? 0)
        if drawing.running == nil, drawing.edgeSpeed() != 0 { runAtEdge() }
    }

    /// The pointer at a height in the list stretches the pick to the line
    /// under it, holding at the file's first or last line.
    private func reach(_ y: CGFloat) {
        guard let file = drawing.file, let index = drawing.line(at: y, within: file), index != drawing.reached,
            review.page.rows.indices.contains(index)
        else { return }
        drawing.reached = index
        review.extend(to: index)
        head = review.page.rows[index].key
    }

    /// The pointer let go: what was picked gets its comment field, as a
    /// click's does.
    private func finishDrag() {
        let picked = drawing.file != nil
        drawing.end()
        if picked { review.writing = true }
    }

    /// Runs the list under a pointer held at an edge, a frame at a time,
    /// stretching the pick to the line that comes under it, until the
    /// pointer leaves the edge, the list ends or the button is let go.
    private func runAtEdge() {
        drawing.running = Task { @MainActor in
            // The offset is kept here rather than read back: the list says
            // where it went a frame after it is told.
            var at = drawing.scroll?.contentOffset.y ?? 0
            var last = ContinuousClock.now
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(16))
                let speed = drawing.edgeSpeed()
                guard speed != 0, let pointer = drawing.pointer, let scroll = drawing.scroll else { break }
                let now = ContinuousClock.now
                let elapsed = (now - last).components
                last = now
                let seconds = Double(elapsed.seconds) + Double(elapsed.attoseconds) * 1e-18
                let lowest = -scroll.contentInsets.top
                let highest = max(
                    lowest, scroll.contentSize.height - scroll.containerSize.height + scroll.contentInsets.bottom)
                let offset = min(highest, max(lowest, at + speed * seconds))
                guard offset != at else { break }
                at = offset
                position.scrollTo(y: offset + scroll.contentInsets.top)
                reach(pointer + offset)
            }
            drawing.running = nil
        }
    }

    /// Moves the selection a row up or down from where the keys last left
    /// it, over the lines and the files' heads, or takes in that row.
    private func move(by step: Int, extending: Bool) {
        let rows = review.page.rows
        let from = head.flatMap { key in rows.firstIndex { $0.key == key } }
            ?? review.selected.map { step > 0 ? $0.upperBound : $0.lowerBound }
        var at = from.map { $0 + step } ?? (step > 0 ? 0 : rows.count - 1)
        while rows.indices.contains(at), !Self.selectable(rows[at]) { at += step }
        guard rows.indices.contains(at) else { return }
        head = rows[at].key
        if extending, review.selected != nil {
            review.extend(to: at)
        } else {
            review.writing = false
            review.select(at)
        }
    }

    /// A line can be commented on, and a file's head for the whole file.
    private static func selectable(_ row: ReviewRow) -> Bool {
        switch row.of {
        case .line, .file: true
        default: false
        }
    }

    // MARK: - Writing

    /// The field for a comment sits under the last row picked.
    private func writer(after row: Int) -> Bool {
        review.writing && review.selected?.upperBound == row
    }

    @ViewBuilder private func writerRow(_ page: ReviewPage) -> some View {
        if let anchor = review.anchor {
            CommentWriter(
                on: anchor.start == 0
                    ? ChatWords.reviewCommentAt(.file) ?? ""
                    : ChatWords.reviewLines(start: anchor.start, end: anchor.end),
                words: Binding(get: { review.words }, set: { review.words = $0 }),
                save: { review.comment(review.words) },
                cancel: { review.writing = false })
        } else {
            Text(ChatWords.reviewOneFile)
                .font(.ui(.meta))
                .foregroundStyle(Palette.textTertiary)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
        }
    }
}

/// The page's rows a file at a time: its head, which stays at the top while
/// its rows scroll under it, then its rows.
struct FileSection: Identifiable {
    struct Item: Identifiable {
        let index: Int
        let id: String
    }

    let id: String
    /// The head's index; none for rows before the first file.
    let head: Int?
    var rows: [Item]

    /// The rows of the file a row is in: from its head to the next.
    static func range(of row: Int, in rows: [ReviewRow]) -> Range<Int> {
        let isHead = { (at: Int) -> Bool in
            if case .file = rows[at].of { return true }
            return false
        }
        var start = row
        while start > 0, !isHead(start) { start -= 1 }
        var end = row + 1
        while end < rows.count, !isHead(end) { end += 1 }
        return start..<end
    }

    static func of(_ rows: [ReviewRow]) -> [FileSection] {
        var sections: [FileSection] = []
        for (index, row) in rows.enumerated() {
            if case .file = row.of {
                sections.append(FileSection(id: row.key, head: index, rows: []))
            } else {
                if sections.isEmpty { sections.append(FileSection(id: "", head: nil, rows: [])) }
                sections[sections.count - 1].rows.append(Item(index: index, id: row.key))
            }
        }
        return sections
    }
}

/// What a drag picking lines needs on every movement and every scrolled
/// frame. Held by reference and not observed, so neither redraws the page.
@MainActor
final class Drawing {
    /// Each drawn line's frame in the list, by its row's key.
    var frames: [String: CGRect] = [:]
    /// Each row's index in the page, by key, as the drag began.
    var places: [String: Int] = [:]
    var scroll: ScrollGeometry?
    /// The pointer's height in the list's visible frame, while it drags.
    var pointer: CGFloat?
    /// The rows of the file the pick began in.
    var file: Range<Int>?
    /// The line the pick reaches now.
    var reached: Int?
    /// The press was not on a line, so this drag picks nothing.
    var missed = false
    /// The list running under a pointer held at an edge.
    var running: Task<Void, Never>?

    /// How deep the band at each edge is where the list runs.
    static let edge: CGFloat = 36
    /// How fast it runs with the pointer at or past the edge, in points a
    /// second, and as the pointer enters the band.
    static let fastest: CGFloat = 1200
    static let slowest: CGFloat = 120

    func end() {
        running?.cancel()
        running = nil
        pointer = nil
        file = nil
        reached = nil
        missed = false
    }

    /// The line under a height in the list. Within a file, a pointer past
    /// the file's first or last drawn line holds at it, and one over a
    /// comment or a hunk's band holds where it was.
    func line(at y: CGFloat, within file: Range<Int>?) -> Int? {
        let lines = frames.compactMap { key, frame in places[key].map { ($0, frame) } }
            .filter { file?.contains($0.0) ?? true }
        if let under = lines.first(where: { $0.1.minY <= y && y < $0.1.maxY }) { return under.0 }
        guard file != nil else { return nil }
        if let first = lines.min(by: { $0.0 < $1.0 }), y < first.1.minY { return first.0 }
        if let last = lines.max(by: { $0.0 < $1.0 }), y >= last.1.maxY { return last.0 }
        return nil
    }

    /// How fast the list runs under the pointer, from how far into an edge
    /// band it is: up at the top, down at the bottom, none between.
    func edgeSpeed() -> CGFloat {
        guard let pointer, let scroll, file != nil else { return 0 }
        let top = scroll.contentInsets.top
        let bottom = scroll.containerSize.height - scroll.contentInsets.bottom
        let into: CGFloat
        if pointer < top + Self.edge {
            into = -(top + Self.edge - pointer)
        } else if pointer > bottom - Self.edge {
            into = pointer - (bottom - Self.edge)
        } else {
            return 0
        }
        let depth = min(1, abs(into) / Self.edge)
        let speed = Self.slowest + (Self.fastest - Self.slowest) * depth
        return into < 0 ? -speed : speed
    }
}

/// "All changes ⌄": the view shown, and a menu of the views where a round
/// has been sent; with only one, what it compares against ("Since main").
private struct ViewSwitch: View {
    let header: ReviewHeader
    let show: (ReviewView) -> Void

    var body: some View {
        let label = HStack(spacing: 4) {
            Text(header.views.count > 1 ? ChatWords.reviewView(header.view) : ChatWords.reviewAgainst(header))
                .font(.ui(.body, weight: .medium))
                .lineLimit(1)
            if header.views.count > 1 {
                Icon("chevron-down", size: 11).foregroundStyle(Palette.textTertiary)
            }
        }
        .padding(.horizontal, 6)
        .frame(height: 28)
        if header.views.count > 1 {
            Menu {
                ForEach(header.views, id: \.view) { tab in
                    Toggle(isOn: Binding(get: { tab.view == header.view }, set: { _ in show(tab.view) })) {
                        Text(ChatWords.reviewView(tab.view))
                        Text(ChatWords.files(Int(tab.files)))
                    }
                }
            } label: {
                label
            }
            .menuStyle(.button)
            .buttonStyle(HoverButtonStyle())
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Switch between all changes and what changed since your last review")
        } else {
            label
        }
    }
}

/// Something the page says above the files, with what can be done about
/// it.
private struct BannerRow: View {
    let banner: ReviewBanner
    let act: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Icon(Self.symbol(banner), size: 13).foregroundStyle(tone)
            Text(ChatWords.reviewBanner(banner))
                .foregroundStyle(Palette.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 6)
            if let action = ChatWords.reviewBannerAction(banner) {
                Button(action, action: act).buttonStyle(PillButtonStyle(small: true))
            }
        }
        .font(.ui(.meta))
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.inset)
        .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 1) }
    }

    /// Failure in red; everything else is news, not alarm.
    private var tone: Color {
        if case .snapshotFailed = banner { return Palette.failure }
        return Palette.textSecondary
    }

    static func symbol(_ banner: ReviewBanner) -> String {
        switch banner {
        case .changedSinceYouStarted, .changedSince: "refresh-cw"
        case .movedDrafts: "corner-down-right"
        case .mergeUnderway: "git-branch"
        case .roundGone: "triangle-alert"
        case .snapshotFailed: "circle-alert"
        case .unreachable: "cloud-off"
        case .notOffered: "ban"
        case .notARepository: "folder"
        }
    }
}

/// A page with nothing in it, said in its middle, with what to do next.
private struct CentreNote: View {
    let symbol: String
    let text: String
    var action: String?
    var act: () -> Void = {}

    var body: some View {
        VStack(spacing: 10) {
            Icon(symbol, size: 22).foregroundStyle(Palette.textTertiary)
            Text(text)
                .font(.ui(.body))
                .foregroundStyle(Palette.textSecondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 320)
                .fixedSize(horizontal: false, vertical: true)
            if let action {
                Button(action, action: act).buttonStyle(PillButtonStyle(small: true))
            }
        }
    }
}

/// Over the earlier comments when nothing changed since the last review.
private struct NothingSince: View {
    let all: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Text(ChatWords.reviewEmpty(.nothingSince)).foregroundStyle(Palette.textSecondary)
            Spacer(minLength: 6)
            Button(ChatWords.reviewView(.all), action: all).buttonStyle(PillButtonStyle(small: true))
        }
        .font(.ui(.meta))
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}

/// A file's head: fold, its name, why it starts folded, its comments, and
/// its lines added and removed in the view shown. A click folds or opens
/// it; its menu comments on the whole file.
private struct FileHeadRow: View {
    let file: ReviewFileHead
    let view: ReviewView?
    let rounds: UInt32
    let selected: Bool
    let toggle: () -> Void
    let commentOnFile: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 6) {
                Icon(file.folded ? "chevron-right" : "chevron-down", size: 12)
                    .foregroundStyle(Palette.textTertiary)
                    .frame(width: 12)
                FileName(path: file.path, from: file.oldPath)
                    .layoutPriority(1)
                ForEach(file.flags, id: \.self) { flag in
                    Text(ChatWords.reviewFlag(flag)).foregroundStyle(Palette.textTertiary).lineLimit(1)
                }
                if !file.inChange {
                    Text(ChatWords.reviewOutsideChange).foregroundStyle(Palette.textTertiary).lineLimit(1)
                } else if view == .all, rounds > 0, file.changedSinceLast {
                    Text(ChatWords.reviewChangedSinceLast).foregroundStyle(Palette.textTertiary).lineLimit(1)
                }
                Spacer(minLength: 6)
                if file.comments > 0 {
                    HStack(spacing: 3) {
                        Icon("message-square-text", size: 12)
                        Text("\(file.comments)").monospacedDigit()
                    }
                    .foregroundStyle(Palette.textSecondary)
                    .help(ChatWords.comments(Int(file.comments)))
                }
                if file.status == .added { Text("new").foregroundStyle(Palette.added) }
                if file.status == .deleted { Text("deleted").foregroundStyle(Palette.removed) }
                if file.added > 0 { Text("+\(file.added)").foregroundStyle(Palette.added).monospacedDigit() }
                if file.removed > 0 { Text("−\(file.removed)").foregroundStyle(Palette.removed).monospacedDigit() }
            }
            .font(.ui(.meta))
            .padding(.horizontal, 12)
            .frame(height: 34)
            .background(Palette.content)
            .overlay(selected ? Palette.accent.opacity(0.14) : .clear)
            .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 1) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button("Comment on the File", action: commentOnFile)
            Button(file.folded ? "Show Changes" : "Fold", action: toggle)
        }
    }
}

/// Where a hunk sits in the code: `impl Store › fn open`.
private struct HunkHeadRow: View {
    let hunk: ReviewHunkHead

    var body: some View {
        Text(hunk.scope.isEmpty ? "⋯" : hunk.scope.joined(separator: " › "))
            .font(.mono(.small))
            .foregroundStyle(Palette.textTertiary)
            .lineLimit(1)
            .truncationMode(.head)
            .padding(.horizontal, 12)
            .padding(.vertical, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.hover)
    }
}

/// Lines between what is shown: how many, and the asks that load more of
/// them, up from the code below or down from the code above.
private struct GapRow: View {
    let gap: ReviewGap
    let more: (ReviewContextAsk) -> Void

    var body: some View {
        HStack(spacing: 4) {
            Text(ChatWords.reviewGap(gap))
                .foregroundStyle(Palette.textTertiary)
                .lineLimit(1)
                .padding(.leading, 12)
            Spacer(minLength: 6)
            if gap.loading {
                ProgressView().controlSize(.mini).padding(.trailing, 6)
            } else {
                if let down = gap.down { ask(down, up: false) }
                if let up = gap.up { ask(up, up: true) }
            }
        }
        .font(.ui(.meta))
        .padding(.trailing, 6)
        .frame(height: 28)
        .background(Palette.inset)
    }

    private func ask(_ ask: ReviewContextAsk, up: Bool) -> some View {
        Button {
            more(ask)
        } label: {
            HStack(spacing: 4) {
                Icon(up ? "arrow-up" : "arrow-down", size: 11)
                Text(ChatWords.reviewMore(ask, up: up)).lineLimit(1)
            }
            .foregroundStyle(Palette.textSecondary)
            .padding(.horizontal, 6)
            .frame(height: 22)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
    }
}

/// One line of code, with its numbers: the compared commit's and the
/// snapshot's. A changed line takes its tint across the numbers, with a
/// bar of its colour down the edge, and the words that differ from its
/// partner on the other side are marked more strongly. A line a comment
/// covers has its numbers in the accent.
private struct CodeLineRow: View {
    let line: ReviewCodeLine
    let selected: Bool
    /// Lines can be picked here to comment on; a round only reads.
    var picks = true
    @State private var hovering = false

    var body: some View {
        // Wrapped, never cut, hanging under its own indent, as on the
        // review over a patch.
        let (indent, code) = PatchText.split(line.text)
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(line.oldLine.map(String.init) ?? "")
                .frame(width: 32, alignment: .trailing)
            Text(line.newLine.map(String.init) ?? "")
                .frame(width: 32, alignment: .trailing)
                .padding(.trailing, 6)
            Text(sign).frame(width: 16).foregroundStyle(signTint)
            if !indent.isEmpty { Text(indent).fixedSize() }
            Text(Self.code(line, dropping: line.text.count - code.count))
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .foregroundStyle(line.commented ? Palette.accent : Palette.textTertiary)
        .font(.mono(.small))
        .padding(.vertical, 1)
        .background(tint)
        .overlay(alignment: .leading) {
            switch line.kind {
            case .added: Rectangle().fill(Palette.added).frame(width: 2)
            case .removed: Rectangle().fill(Palette.removed).frame(width: 2)
            case .context: EmptyView()
            }
        }
        .overlay(selected ? Palette.accent.opacity(0.14) : (hovering ? Palette.hover : .clear))
        .contentShape(Rectangle())
        .onHover { if picks { hovering = $0 } }
        .help(picks ? "Click or drag to comment. Shift-click to take in more lines." : "")
    }

    /// The code after its indent: each run in its token's colour, and the
    /// changed words on the line's green or red. A line too long for the
    /// pane wraps between tokens: text breaks only at spaces, and code like
    /// `self.hello.wait().await` has none, so without a break offered at
    /// each token's edge such a line would be cut mid-word wherever the
    /// width ran out.
    static func code(_ line: ReviewCodeLine, dropping count: Int) -> AttributedString {
        guard !line.runs.isEmpty else {
            var plain = AttributedString(breakable(String(line.text.dropFirst(count))))
            plain.foregroundColor = Palette.text
            return plain
        }
        var left = count
        var text = AttributedString()
        for run in line.runs {
            let cut = min(left, run.text.count)
            left -= cut
            let rest = String(run.text.dropFirst(cut))
            guard !rest.isEmpty else { continue }
            var piece = AttributedString(breakable(rest) + Self.mayBreak)
            piece.foregroundColor = Palette.syntax(run.kind) ?? Palette.text
            if run.changed {
                piece.backgroundColor = (line.kind == .removed ? Palette.removed : Palette.added).opacity(0.26)
            }
            text += piece
        }
        return text
    }

    /// Where a line may wrap, drawn as nothing.
    static let mayBreak = "\u{200B}"

    /// `code` with a place to wrap after each punctuation mark that ends a
    /// token: a path's dots and colons, a call's brackets, a list's commas.
    static func breakable(_ code: String) -> String {
        var out = ""
        out.reserveCapacity(code.count)
        for char in code {
            out.append(char)
            if ".,:;([{)]}/".contains(char) { out += mayBreak }
        }
        return out
    }

    private var sign: String {
        switch line.kind {
        case .added: "+"
        case .removed: "−"
        case .context: " "
        }
    }

    private var signTint: Color {
        switch line.kind {
        case .added: Palette.added
        case .removed: Palette.removed
        case .context: Palette.textSecondary
        }
    }

    private var tint: Color {
        switch line.kind {
        case .added: Palette.added.opacity(PatchText.wash)
        case .removed: Palette.removed.opacity(PatchText.wash)
        case .context: .clear
        }
    }
}

/// A comment where the page places it: whether it is a draft or which
/// round sent it, where it sits when not under its lines, the code it was
/// written on when that is not drawn above it, and its words. An outdated
/// one is folded to a line until opened. Under the pointer it offers to
/// edit or delete it, where it can be changed.
private struct CommentCard: View {
    let comment: ReviewCommentRow
    var toggle: () -> Void = {}
    var reword: ((String) -> Void)?
    var delete: (() -> Void)?
    @State private var hovering = false
    @State private var editing: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            heading
            if !comment.folded {
                if !comment.scope.isEmpty {
                    Text(comment.scope.joined(separator: " › "))
                        .font(.mono(.caption))
                        .foregroundStyle(Palette.textTertiary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                if !comment.snippet.isEmpty { snippet }
                if let editing {
                    CommentWriter(
                        on: nil, words: Binding(get: { editing }, set: { self.editing = $0 }),
                        save: {
                            reword?(editing)
                            self.editing = nil
                        },
                        cancel: { self.editing = nil }, framed: false, verb: "Save")
                } else {
                    Text(comment.text)
                        .font(.ui(.body))
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).strokeBorder(Palette.line))
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .onHover { hovering = $0 }
    }

    /// "Draft", "Sent in round 1 · Outdated", with the folded words after
    /// it and the actions under the pointer.
    private var heading: some View {
        HStack(spacing: 6) {
            if comment.at == .outdated {
                Button(action: toggle) {
                    Icon(comment.folded ? "chevron-right" : "chevron-down", size: 11)
                        .foregroundStyle(Palette.textTertiary)
                        .frame(width: 12)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(comment.folded ? "Show the comment" : "Fold the comment")
            }
            Icon("message-square-text", size: 13)
                .foregroundStyle(comment.state == .draft ? Palette.accent : Palette.textTertiary)
            Text(words)
                .foregroundStyle(Palette.textSecondary)
                .lineLimit(1)
                .layoutPriority(1)
            if comment.folded {
                Text(comment.text).foregroundStyle(Palette.textTertiary).lineLimit(1)
            }
            Spacer(minLength: 4)
            if hovering, editing == nil, reword != nil {
                Button { editing = comment.text } label: { Icon("pencil", size: 12) }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.textSecondary)
                    .help("Edit")
            }
            if hovering, editing == nil, let delete {
                Button(action: delete) { Icon("trash", size: 12) }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.textSecondary)
                    .help("Delete")
            }
        }
        .font(.ui(.meta))
        .contentShape(Rectangle())
        .onTapGesture { if comment.at == .outdated { toggle() } }
    }

    private var words: String {
        var parts = [ChatWords.reviewCommentState(comment.state)]
        if let at = ChatWords.reviewCommentAt(comment.at) { parts.append(at) }
        if case .lines(let start, let end) = comment.at, !comment.snippet.isEmpty {
            parts.append(ChatWords.reviewLines(start: start, end: end))
        }
        if comment.changedNear { parts.append(ChatWords.reviewChangedNear) }
        return parts.joined(separator: " · ")
    }

    private var snippet: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(comment.snippet.enumerated()), id: \.offset) { _, line in
                let (indent, code) = PatchText.split(line.text)
                HStack(alignment: .firstTextBaseline, spacing: 0) {
                    Text(line.kind == .added ? "+" : (line.kind == .removed ? "−" : " "))
                        .frame(width: 14)
                        .foregroundStyle(
                            line.kind == .added
                                ? Palette.added : (line.kind == .removed ? Palette.removed : Palette.textTertiary))
                    if !indent.isEmpty { Text(indent).fixedSize() }
                    Text(CodeLineRow.code(line, dropping: line.text.count - code.count))
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                }
                .font(.mono(.small))
                .padding(.vertical, 1)
            }
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 4)
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.inset))
    }
}

/// Where a comment is written: what it is on, a field that grows with it
/// (Return saves, Shift-Return starts a line, Escape steps back), and
/// Cancel and Comment.
private struct CommentWriter: View {
    /// "Lines 41–42", "On the file"; nil when editing a comment in place.
    let on: String?
    @Binding var words: String
    let save: () -> Void
    let cancel: () -> Void
    var framed = true
    var verb = "Comment"
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let on {
                Text(on).font(.ui(.caption)).foregroundStyle(Palette.textTertiary)
            }
            TextField("Add a comment", text: $words, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...8)
                .focused($focused)
                .onKeyPress(.return, phases: .down) { press in
                    if press.modifiers.contains(.shift) || press.modifiers.contains(.option) {
                        words += "\n"
                    } else if !blank {
                        save()
                    }
                    return .handled
                }
                .onKeyPress(.escape) {
                    cancel()
                    return .handled
                }
            HStack {
                Spacer()
                Button("Cancel", action: cancel).buttonStyle(PillButtonStyle(small: true))
                Button(verb, action: save)
                    .buttonStyle(PillButtonStyle(prominent: true, small: true))
                    .disabled(blank)
            }
        }
        .font(.ui(.body))
        .padding(framed ? 10 : 0)
        .background {
            if framed {
                RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.card)
            }
        }
        .overlay {
            if framed {
                RoundedRectangle(cornerRadius: Radius.card, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.16))
            }
        }
        .padding(.horizontal, framed ? 10 : 0)
        .padding(.vertical, framed ? 4 : 0)
        .onAppear { focused = true }
    }

    private var blank: Bool { words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
}

/// The round a review sent into the chat closed, as a sheet over the
/// window from its chip: each comment it sent under its file, with the
/// code it was written on as stored with it, read from the agent's host.
/// When the host no longer holds the round, as after starting over, it
/// shows the text the agent read. The review as it is now is one click
/// away.
public struct ReviewRoundSheet: View {
    /// The round the chip expected; the host's own number once read.
    let round: UInt32
    /// What the agent read.
    let text: String
    let read: @MainActor () async -> ReviewPage?
    let openReview: @MainActor () -> Void
    let close: @MainActor () -> Void
    @State private var asked: Bool
    @State private var page: ReviewPage?

    public init(
        round: UInt32, text: String, page: ReviewPage? = nil, read: @escaping @MainActor () async -> ReviewPage?,
        openReview: @escaping @MainActor () -> Void, close: @escaping @MainActor () -> Void
    ) {
        self.round = round
        self.text = text
        self.read = read
        self.openReview = openReview
        self.close = close
        _page = State(initialValue: page)
        _asked = State(initialValue: page != nil)
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Icon("message-square-text", size: 16).foregroundStyle(Palette.textSecondary)
                Text(ChatWords.reviewRound(number)).font(.ui(.title, weight: .semibold))
                Spacer()
            }
            .padding(.horizontal, 20)
            .frame(height: 52)
            .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 1) }
            content.frame(maxWidth: .infinity, maxHeight: .infinity)
            HStack(spacing: 8) {
                Spacer()
                if asked {
                    Button(ChatWords.reviewOpenReview) {
                        close()
                        openReview()
                    }
                    .buttonStyle(PillButtonStyle())
                    .help("Show the agent’s changes as they are now")
                }
                Button("Done", action: close)
                    .buttonStyle(PillButtonStyle(prominent: true))
                    .keyboardShortcut(.defaultAction)
            }
            .padding(16)
            .overlay(alignment: .top) { Rectangle().fill(Palette.line).frame(height: 1) }
        }
        .frame(width: 520, height: 560)
        .background(Palette.content)
        .onExitCommand(perform: close)
        .task {
            guard !asked else { return }
            page = await read()
            asked = true
        }
    }

    /// "Round 2", as the agent's host numbered it when the message arrived.
    private var number: UInt32 {
        page?.rows.lazy.compactMap { row -> UInt32? in
            if case .comment(let comment) = row.of, case .sent(let round) = comment.state { return round }
            return nil
        }.first ?? round
    }

    /// Why the sheet shows the text the agent read rather than the round:
    /// the host holds none of it, or cannot be reached and none of it was
    /// loaded. Nil while it shows the round.
    private var instead: String? {
        guard let page else { return ChatWords.reviewRoundGone }
        let unloaded = page.header == nil && page.banners.contains(.unreachable)
        return unloaded ? ChatWords.reviewRoundUnreachable : nil
    }

    @ViewBuilder private var content: some View {
        if !asked {
            VStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text(ChatWords.reviewOpening).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
            }
        } else if let page, instead == nil {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    ForEach(Array(page.banners.enumerated()), id: \.offset) { _, banner in
                        BannerRow(banner: banner, act: {})
                    }
                    ForEach(FileSection.of(page.rows)) { section in
                        Section {
                            ForEach(section.rows) { item in row(page.rows[item.index]) }
                        } header: {
                            if let at = section.head, case .file(let file) = page.rows[at].of {
                                HStack(spacing: 6) {
                                    FileName(path: file.path, from: file.oldPath).layoutPriority(1)
                                    if !file.inChange {
                                        Text(ChatWords.reviewOutsideChange)
                                            .foregroundStyle(Palette.textTertiary)
                                            .lineLimit(1)
                                    }
                                    Spacer(minLength: 6)
                                }
                                .font(.ui(.meta))
                                .padding(.horizontal, 12)
                                .frame(height: 34)
                                .background(Palette.content)
                                .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 1) }
                            }
                        }
                    }
                }
                .padding(.bottom, 12)
            }
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(instead ?? ChatWords.reviewRoundGone)
                        .font(.ui(.body))
                        .foregroundStyle(Palette.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(text)
                        .font(.mono(.small))
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(20)
            }
        }
    }

    /// A round's rows are its whole diff under its files' heads, with its
    /// comments; drawn as the page draws them, to read only.
    @ViewBuilder private func row(_ row: ReviewRow) -> some View {
        switch row.of {
        case .comment(let comment): CommentCard(comment: comment)
        case .line(let line): CodeLineRow(line: line, selected: false, picks: false)
        case .hunk(let hunk): HunkHeadRow(hunk: hunk)
        case .noLines(_, let why):
            Text(ChatWords.reviewNoLines(why))
                .font(.ui(.meta))
                .foregroundStyle(Palette.textTertiary)
                .padding(.horizontal, 12)
                .frame(height: 30)
        case .file, .gap, .loading: EmptyView()
        }
    }
}
