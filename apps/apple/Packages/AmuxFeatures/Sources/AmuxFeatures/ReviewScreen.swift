import AmuxCore
import AmuxDesign
import AmuxWords
import SwiftUI

/// The agent's review, read and commented on with a thumb.
///
/// Every row is the shared page's, drawn where the page puts it: a file's
/// head, which stays pinned while its file is read and folds it; where a
/// hunk sits in the code; its lines, with the words a line changed marked;
/// gaps that load more of the file around a change; and the person's
/// comments under the lines they are on, or under the file's head. Nothing
/// is worked out again here.
///
/// A tap on a line selects it, a tap on another line of the same file
/// stretches the selection to it, and holding a line then dragging draws
/// one; a file's head is held for a comment on the whole file. The page's
/// foot then offers the comment, which is written in a sheet that keeps the
/// lines in sight. Tapping a comment edits or deletes it. Drafts wait on the
/// agent's computer and show as a chip in the chat's composer, which sends
/// them with the next message; Done goes back there.
public struct ReviewScreen: View {
    @Environment(\.design) private var design
    let session: ReviewSession
    /// Back to the chat, where the chip sends the drafts.
    let done: () -> Void
    @State private var writing = false
    /// The comment being edited.
    @State private var editing: ReviewCommentRow?
    @State private var note = ""
    @State private var asking: Asking?
    /// Each drawn line's frame in the list, by row index, so a finger's
    /// place names a row.
    @State private var frames: [Int: CGRect] = [:]
    /// A row to bring to the top.
    @State private var target: String?
    /// Files whose rows are on screen, so the file menu ticks the one read.
    @State private var visible: Set<String> = []
    @State private var screenWidth: CGFloat?

    public init(
        session: ReviewSession, writing: Bool = false, editing: ReviewCommentRow? = nil,
        done: @escaping () -> Void
    ) {
        self.session = session
        self.done = done
        _writing = State(initialValue: writing)
        _editing = State(initialValue: editing)
        _note = State(initialValue: editing?.text ?? "")
    }

    /// What the person is asked to confirm before it happens.
    enum Asking: Identifiable {
        case discardDrafts(UInt32)
        case startOver
        var id: String {
            switch self {
            case .discardDrafts: "discard"
            case .startOver: "start-over"
            }
        }
    }

    static let space = "review"

    private var page: ReviewPage { session.page }

    public var body: some View {
        ScrollViewReader { proxy in
            ZStack(alignment: .bottom) {
                Ground()
                content
                foot
                if writing || editing != nil { sheet(proxy) }
            }
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { screenWidth = $0 }
            .onChange(of: target) { _, key in
                guard let key else { return }
                proxy.scrollTo(key, anchor: .top)
                target = nil
            }
        }
        .backBar()
        .toolbar { bar }
        .confirmationDialog(
            question, isPresented: Binding(get: { asking != nil }, set: { if !$0 { asking = nil } }),
            titleVisibility: .visible, presenting: asking
        ) { asked in
            switch asked {
            case .discardDrafts:
                Button(String(localized: "Discard Drafts"), role: .destructive) { session.discardDrafts() }
                    .identified("review.discard.confirm", label: String(localized: "Discard Drafts"))
            case .startOver:
                Button(String(localized: "Start Over"), role: .destructive) { session.startOver() }
                    .identified("review.startOver.confirm", label: String(localized: "Start Over"))
            }
        } message: { asked in
            switch asked {
            case .discardDrafts: Text(ChatWords.discardDraftsDetail)
            case .startOver: Text(ChatWords.startOverDetail)
            }
        }
        .accessibilityElement(children: .contain)
        .reported("review", value: page.header.map { "\($0.drafts) drafts" } ?? "opening")
    }

    private var question: String {
        switch asking {
        case .discardDrafts(let count)?: ChatWords.discardDrafts(count)
        case .startOver?: ChatWords.startOverQuestion
        case nil: ""
        }
    }

    // MARK: - The bar

    @ToolbarContentBuilder
    private var bar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) { title }
        if page.header != nil {
            ToolbarItem(placement: .topBarTrailing) { files }
            ToolbarItem(placement: .topBarTrailing) { more }
        }
    }

    /// The title pill's width at most: what the bar leaves once the way back
    /// and the two buttons are drawn. The bar does not shrink an item to fit;
    /// given more than its width, it folds every item into its own menu.
    private var titleWidth: CGFloat {
        guard let screenWidth else { return 130 }
        return min(220, max(96, screenWidth - 260))
    }

    /// What the title says under "Review": what the change is counted
    /// against, or with the two views to switch between, which the switch
    /// already names, how many files the view shows.
    private func subtitle(_ header: ReviewHeader) -> String {
        header.views.count > 1 ? ChatWords.files(Int(header.files)) : ChatWords.reviewAgainst(header)
    }

    /// "Review" over what it is against and its lines: "Since main · +12
    /// −4". A tap goes back to the top, as tapping a bar does.
    private var title: some View {
        Button {
            target = Self.top
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                Text("Review")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                if let header = page.header {
                    HStack(spacing: Space.xs) {
                        Text(verbatim: subtitle(header))
                            .foregroundStyle(design.inkMuted.color)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Delta(added: header.added, removed: header.removed)
                    }
                    .designFont(.caption, design)
                    .monospacedDigit()
                }
            }
            // The bar does not grow with the text: past the largest ordinary
            // size the pill would no longer fit it.
            .dynamicTypeSize(...DynamicTypeSize.xLarge)
            .padding(.horizontal, Space.m)
            .modifier(Capped(width: titleWidth))
            .frame(height: 36)
        }
        .accessibilityLabel(titleWords)
        .accessibilityHint("Goes to the top")
        .identified("review.summary", label: titleWords)
    }

    private var titleWords: String {
        guard let header = page.header else { return String(localized: "Review") }
        return "\(ChatWords.reviewAgainst(header)), \(ChatWords.reviewCounts(header))"
    }

    /// Every file, to jump to one: its name and what it changed, the one
    /// being read ticked.
    private var files: some View {
        let heads = fileHeads
        let reading = heads.first { visible.contains($0.path) }?.path
        return MenuButton(
            name: String(localized: "Files"), identifier: "review.files",
            items: heads.map { head in
                MenuItem(
                    title: head.path, subtitle: ReviewWords.fileSummary(head),
                    chosen: head.path == reading
                ) { target = "f/\(head.path)" }
            }
        ) {
            Image(systemName: "list.bullet")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(design.ink.color)
                .frame(width: 36, height: 36)
        }
    }

    /// Refreshing, and the two that delete, each asked first.
    private var more: some View {
        Menu {
            Button { session.refresh() } label: {
                Label(String(localized: "Refresh"), systemImage: "arrow.clockwise")
            }
            if let header = page.header, header.discardDrafts {
                Button(role: .destructive) { asking = .discardDrafts(header.drafts) } label: {
                    Label(String(localized: "Discard Drafts"), systemImage: "trash")
                }
            }
            if page.header?.startOver == true {
                Button(role: .destructive) { asking = .startOver } label: {
                    Label(String(localized: "Start Over"), systemImage: "arrow.counterclockwise")
                }
            }
        } label: {
            Image(systemName: "ellipsis")
                .foregroundStyle(design.ink.color)
                .accessibilityLabel("More")
        }
        .accessibilityLabel("More")
        .identified("review.more", label: String(localized: "More"))
    }

    private var fileHeads: [ReviewFileHead] {
        page.rows.compactMap { if case .file(let head) = $0.of { head } else { nil } }
    }

    // MARK: - The page

    static let top = "review.top"

    @ViewBuilder
    private var content: some View {
        if page.header == nil && page.banners.isEmpty {
            PagePlaceholder(.waiting(ChatWords.reviewOpening), identifier: "review")
        } else {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    above.id(Self.top)
                    ForEach(ReviewSections(page.rows).all) { section in
                        Section {
                            ForEach(section.rows, id: \.self) { index in row(index) }
                        } header: {
                            if let head = section.head { fileHead(head) }
                        }
                    }
                }
                .padding(.bottom, 120)
                .coordinateSpace(.named(Self.space))
                .onPreferenceChange(RowFrames.self) { frames = $0 }
                .gesture(selecting)
            }
            .scrollDismissesKeyboard(.interactively)
            .refreshable { _ = await session.open()?.value }
        }
    }

    /// What stands above the files: the switch between the views, what the
    /// page says about itself, and why there is nothing to read.
    @ViewBuilder
    private var above: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            if let header = page.header, header.views.count > 1 {
                Picker(String(localized: "View"), selection: Binding(
                    get: { header.view }, set: { session.show($0) }
                )) {
                    ForEach(header.views, id: \.view) { tab in
                        Text(ChatWords.reviewView(tab.view)).tag(tab.view)
                    }
                }
                .pickerStyle(.segmented)
                .identified("review.views", label: ChatWords.reviewView(header.view), value: header.view.rawValue)
            }
            ForEach(Array(page.banners.enumerated()), id: \.offset) { _, banner in
                ReviewBannerCard(banner: banner) { act(on: banner) }
            }
            if let empty = page.empty { emptied(empty) }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, Space.s)
    }

    private func act(on banner: ReviewBanner) {
        switch banner {
        case .changedSinceYouStarted, .changedSince: session.refresh()
        case .snapshotFailed: session.open()
        default: break
        }
    }

    /// Nothing to review at all, or nothing since the last round, with all
    /// changes on the switch above.
    private func emptied(_ empty: ReviewEmpty) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(ChatWords.reviewEmpty(empty))
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .identified("review.empty", label: ChatWords.reviewEmpty(empty), value: empty.rawValue)
    }

    // MARK: - Rows

    private func fileHead(_ index: Int) -> some View {
        let row = page.rows[index]
        guard case .file(let head) = row.of else { return AnyView(EmptyView()) }
        let selected = session.isSelected(index)
        return AnyView(
            ReviewFileHeadRow(
                head: head, selected: selected, marksSince: page.header?.view == .all
            ) {
                session.toggleFile(head.path)
            }
            .id(row.key)
            .contextMenu {
                Button { commentOnFile(index) } label: {
                    Label(String(localized: "Comment on the File"), systemImage: "text.bubble")
                }
                Button { session.toggleFile(head.path) } label: {
                    Label(head.folded ? String(localized: "Open") : String(localized: "Fold"),
                          systemImage: head.folded ? "chevron.down" : "chevron.right")
                }
            }
            .accessibilityAction(named: Text("Comment on the File")) { commentOnFile(index) }
            .identified(
                "review.file", label: head.path,
                value: [head.folded ? "folded" : "open", selected ? "selected" : nil]
                    .compactMap { $0 }.joined(separator: " "))
            .onScrollVisibilityChange(threshold: 0.01) { shown in
                if shown { visible.insert(head.path) } else { visible.remove(head.path) }
            }
        )
    }

    /// A file's head held for a comment: the selection is the head alone,
    /// which the page reads as the whole file.
    private func commentOnFile(_ index: Int) {
        session.select(index)
        note = ""
        writing = true
    }

    @ViewBuilder
    private func row(_ index: Int) -> some View {
        let row = page.rows[index]
        switch row.of {
        case .file:
            EmptyView()
        case .comment(let comment):
            Button {
                if comment.folded {
                    session.toggleComment(comment.id)
                } else {
                    note = comment.text
                    editing = comment
                }
            } label: {
                ReviewCommentCard(comment: comment)
            }
            .buttonStyle(.amuxPush)
            .id(row.key)
            .accessibilityHint(comment.folded ? "Opens the comment" : "Edits the comment")
            .identified("review.comment", label: comment.text, value: ReviewWords.commentValue(comment))
        case .hunk(let hunk):
            ReviewHunkBand(hunk: hunk).id(row.key)
        case .line(let line):
            let selected = session.isSelected(index)
            ReviewCodeLineRow(line: line, selected: selected)
                .background(GeometryReader { geometry in
                    Color.clear.preference(
                        key: RowFrames.self, value: [index: geometry.frame(in: .named(Self.space))])
                })
                .id(row.key)
                .onTapGesture { tap(index) }
                .accessibilityAction(named: Text("Select line")) { tap(index) }
                .identified(
                    "review.line", label: line.text,
                    value: [line.commented ? "commented" : nil, selected ? "selected" : nil]
                        .compactMap { $0 }.joined(separator: " "))
        case .gap(let gap):
            ReviewGapRow(gap: gap) { session.moreContext($0) }.id(row.key)
        case .loading:
            ReviewNote(text: String(localized: "Loading…"), loading: true)
                .id(row.key)
                .identified("review.loading", label: String(localized: "Loading…"))
        case .noLines(_, let why):
            ReviewNote(text: ChatWords.reviewNoLines(why))
                .id(row.key)
                .identified("review.noLines", label: ChatWords.reviewNoLines(why))
        }
    }

    /// A tap on a line: it stretches a selection begun in its file to it,
    /// and otherwise selects it alone.
    private func tap(_ index: Int) {
        if let selected = session.selected, session.anchor != nil,
           sameFile(selected.lowerBound, index) {
            if selected == index...index {
                session.clearSelection()
            } else {
                session.extend(to: index)
            }
        } else {
            session.select(index)
        }
    }

    /// The two rows are under one file's head.
    private func sameFile(_ one: Int, _ other: Int) -> Bool {
        let head = { (index: Int) -> Int? in
            page.rows[...min(index, page.rows.count - 1)].lastIndex {
                if case .file = $0.of { true } else { false }
            }
        }
        return head(one) == head(other)
    }

    /// Hold a line for a moment, then drag: the selection follows the
    /// finger. A quick swipe fails the hold and scrolls instead.
    private var selecting: some Gesture {
        LongPressGesture(minimumDuration: 0.3)
            .sequenced(before: DragGesture(minimumDistance: 0, coordinateSpace: .named(Self.space)))
            .onChanged { value in
                guard case .second(true, let drag?) = value,
                      let index = frames.first(where: { $0.value.minY <= drag.location.y && drag.location.y < $0.value.maxY })?.key
                else { return }
                if drag.translation == .zero || session.selected == nil {
                    session.select(index)
                } else {
                    session.extend(to: index)
                }
            }
    }

    // MARK: - The foot

    @ViewBuilder
    private var foot: some View {
        VStack(spacing: 6) {
            if let notice = session.notice {
                Text(notice)
                    .designFont(.detail, design)
                    .foregroundStyle(design.removed.color)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, design.metrics.gutter + 6)
                    .identified("review.notice", label: notice)
            }
            if let selected = session.selected {
                selectionBar(selected)
            } else if let header = page.header, header.drafts > 0 {
                Button(action: done) {
                    Text(ChatWords.reviewDone(drafts: header.drafts))
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ground.color)
                        .frame(maxWidth: .infinity)
                        .frame(height: 50)
                        .background(Capsule().fill(design.ink.color))
                }
                .buttonStyle(.amuxControl)
                .accessibilityHint("Goes back to the chat, where the next message sends the drafts")
                .identified("review.done", label: ChatWords.reviewDone(drafts: header.drafts))
                .padding(.horizontal, design.metrics.gutter)
            }
        }
        .padding(.bottom, 8)
    }

    private func selectionBar(_ selected: ClosedRange<Int>) -> some View {
        let words = commentWords(selected)
        return HStack(spacing: 10) {
            Button { session.clearSelection() } label: {
                Text("Cancel")
                    .designFont(.body, design)
                    .foregroundStyle(design.inkMuted.color)
                    .padding(.horizontal, 16)
                    .frame(height: 44)
            }
            .buttonStyle(.amuxControl)
            .identified("review.deselect", label: String(localized: "Cancel"))
            Spacer(minLength: 0)
            Button {
                note = ""
                writing = true
            } label: {
                Text(verbatim: words)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(session.canComment ? design.ground.color : design.inkFaint.color)
                    .lineLimit(1)
                    .padding(.horizontal, 18)
                    .frame(height: 44)
                    .background(Capsule().fill(session.canComment ? design.ink.color : design.sunken.color))
            }
            .buttonStyle(.amuxControl)
            .disabled(!session.canComment)
            .identified("review.commentOn", label: words, enabled: session.canComment)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .frosted(Capsule(), as: .glass)
        .padding(.horizontal, design.metrics.gutter)
    }

    /// "Comment on 3 lines", "Comment on the file", or why nothing can be
    /// written on what is selected.
    private func commentWords(_ selected: ClosedRange<Int>) -> String {
        guard let anchor = session.anchor else { return ChatWords.reviewOneFile }
        if anchor.start == 0 { return String(localized: "Comment on the File") }
        let lines = selected.filter { index in
            page.rows.indices.contains(index) && { if case .line = page.rows[index].of { true } else { false } }()
        }.count
        return ChatWords.commentOn(lines: max(lines, 1))
    }

    // MARK: - Writing a comment

    private func sheet(_ proxy: ScrollViewProxy) -> some View {
        ZStack(alignment: .bottom) {
            Color.black.opacity(0.25)
                .ignoresSafeArea()
                .onTapGesture { closeSheet() }
                .accessibilityHidden(true)
            if let editing {
                ReviewCommentSheet(
                    title: ChatWords.reviewCommentState(editing.state),
                    action: String(localized: "Save"), actionId: "review.save", note: $note,
                    cancel: closeSheet,
                    add: {
                        session.reword(editing.id, as: note)
                        closeSheet()
                    },
                    remove: {
                        session.delete(editing.id)
                        closeSheet()
                    })
            } else {
                ReviewCommentSheet(
                    title: session.selected.map(commentWords) ?? "",
                    action: String(localized: "Add Comment"), actionId: "review.add", note: $note,
                    cancel: closeSheet,
                    add: {
                        let written = session.comment(note)
                        if written != nil { closeSheet() }
                    })
            }
        }
        .onAppear {
            // The lines being written about stay in sight above the sheet.
            let key = editing.map { "c/" + ReviewWords.hex($0.id) }
                ?? session.selected.flatMap { page.rows.indices.contains($0.upperBound) ? page.rows[$0.upperBound].key : nil }
            if let key { proxy.scrollTo(key, anchor: .init(x: 0.5, y: 0.3)) }
        }
    }

    private func closeSheet() {
        note = ""
        writing = false
        editing = nil
    }
}

/// The page's rows grouped under their files' heads, for the list to pin
/// each head while its rows are read.
struct ReviewSections {
    struct Group: Identifiable {
        let id: String
        /// The head's row; none for rows before the first file.
        let head: Int?
        let rows: [Int]
    }

    let all: [Group]

    init(_ rows: [ReviewRow]) {
        var groups: [Group] = []
        var head: Int?
        var body: [Int] = []
        func close() {
            if head != nil || !body.isEmpty {
                groups.append(Group(id: head.map { rows[$0].key } ?? "above", head: head, rows: body))
            }
        }
        for index in rows.indices {
            if case .file = rows[index].of {
                close()
                head = index
                body = []
            } else {
                body.append(index)
            }
        }
        close()
        all = groups
    }
}

/// The frame each drawn line has in the list, by row index. Only drawn lines
/// report, which are the only ones a finger is on.
private struct RowFrames: PreferenceKey {
    static let defaultValue: [Int: CGRect] = [:]
    static func reduce(value: inout [Int: CGRect], nextValue: () -> [Int: CGRect]) {
        value.merge(nextValue()) { $1 }
    }
}

/// The review's words that only the phone's page uses.
enum ReviewWords {
    /// "+3 −4 · 1 comment", leaving out what is nothing.
    static func fileSummary(_ head: ReviewFileHead) -> String {
        var parts: [String] = []
        let delta = [head.added > 0 ? "+\(head.added)" : nil, head.removed > 0 ? "\u{2212}\(head.removed)" : nil]
            .compactMap { $0 }.joined(separator: " ")
        if !delta.isEmpty { parts.append(delta) }
        if head.comments > 0 { parts.append(ChatWords.comments(Int(head.comments))) }
        return parts.joined(separator: " · ")
    }

    /// What a driver reads of a comment: its state, where it sits, and
    /// whether it is folded.
    static func commentValue(_ comment: ReviewCommentRow) -> String {
        var parts = [ChatWords.reviewCommentState(comment.state)]
        if let at = ChatWords.reviewCommentAt(comment.at) { parts.append(at) }
        if comment.folded { parts.append(String(localized: "Folded")) }
        return parts.joined(separator: " · ")
    }

    /// A comment's id as the page's row key spells it.
    static func hex(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}

/// "+18 −28" in the diff's own green and red, leaving out a side that
/// changed nothing.
struct Delta: View {
    @Environment(\.design) private var design
    let added: UInt32
    let removed: UInt32

    var body: some View {
        HStack(spacing: 4) {
            if added > 0 { Text(verbatim: "+\(added)").foregroundStyle(design.added.color) }
            if removed > 0 { Text(verbatim: "\u{2212}\(removed)").foregroundStyle(design.removed.color) }
        }
        .fixedSize()
    }
}
