import AmuxCore
import AmuxDesign
import SwiftUI

/// What the review page asks of whoever showed it.
public enum ReviewAction: Equatable, Sendable {
    case back
    /// Put the review into the chat's draft and go back to the chat.
    case attach
}

/// An agent's changes, uncommitted or on its branch, read and written about
/// with a finger.
///
/// The files come in the order the host's patch lists them, each under a
/// heading that folds it. Holding a line for a moment and dragging selects
/// lines; a comment goes on the line the selection ends on and is drawn
/// under it. The edge wheel down the right jumps between files, and "All
/// Files" lists them. Attaching puts the whole review into the chat's draft
/// as one token that sends with the reference to the patch it was written
/// against.
public struct ReviewPage: View {
    @Environment(\.design) private var design
    let model: ReviewModel
    let agent: String
    /// What the changes are counted against, as the overview words it:
    /// "Uncommitted", "vs main".
    let against: String?
    let actions: (ReviewAction) -> Void
    @State private var listing = false
    @State private var writing = false
    @State private var note = ""
    @State private var target: Int?
    @State private var frames: [ReviewLine: CGRect] = [:]

    public init(model: ReviewModel, agent: String, against: String? = nil, writing: Bool = false,
                listing: Bool = false, actions: @escaping (ReviewAction) -> Void) {
        self.model = model
        self.agent = agent
        self.against = against
        self.actions = actions
        _writing = State(initialValue: writing)
        _listing = State(initialValue: listing)
    }

    public var body: some View {
        ScrollViewReader { proxy in
            ZStack(alignment: .bottom) {
                Ground()
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(model.doc.files.indices, id: \.self) { index in
                            file(index)
                        }
                    }
                    .padding(.bottom, 96)
                    .coordinateSpace(.named(Self.space))
                    .onPreferenceChange(LineFrames.self) { frames = $0 }
                    .gesture(selecting)
                }
                .scrollDismissesKeyboard(.interactively)
                .overlay(alignment: .trailing) {
                    EdgeWheel(model: model) { proxy.scrollTo(Self.fileId($0), anchor: .top) }
                        .padding(.vertical, 8)
                }
                footer
                if writing { commentSheet(proxy) }
            }
            .onChange(of: target) { _, file in
                guard let file else { return }
                proxy.scrollTo(Self.fileId(file), anchor: .top)
                target = nil
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) { header }
        .toolbar(.hidden, for: .navigationBar)
        .sheet(isPresented: $listing) {
            ReviewFileList(model: model) { index in
                listing = false
                target = index
            }
            .presentationDetents([.medium, .large])
        }
        .accessibilityElement(children: .contain)
        .reported("review", value: "\(model.count)")
    }

    static let space = "review"
    static func fileId(_ index: Int) -> String { "review.file.\(index)" }

    // MARK: - The header

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            BackLink(agent, identifier: "review.back") { actions(.back) }
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text("Review")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                if model.count > 0 {
                    Text(verbatim: "\(model.count)")
                        .designFont(.caption, design)
                        .foregroundStyle(design.onAccent.color)
                        .padding(.horizontal, 7)
                        .frame(minWidth: 20, minHeight: 20)
                        .background(Capsule().fill(design.accent.color))
                        .accessibilityLabel(ChatWords.comments(model.count))
                        .identified("review.count", value: "\(model.count)")
                }
                Spacer(minLength: 0)
            }
            HStack(spacing: 6) {
                Counts(files: model.doc.files.count, added: model.doc.added, removed: model.doc.removed)
                    .identified("review.summary", label: ChatWords.changes(
                        files: model.doc.files.count, added: model.doc.added, removed: model.doc.removed))
                if let against {
                    Text(verbatim: "·").foregroundStyle(design.inkFaint.color)
                    Text(verbatim: against)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                        .identified("review.against", label: against)
                }
            }
            .designFont(.monoSmall, design)
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.top, 2)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(Rectangle(), as: .glass)
    }

    // MARK: - A file

    /// One file as one stacked section: its heading and, unless folded, its
    /// hunks. The list is lazy by file, not by line.
    private func file(_ index: Int) -> some View {
        let file = model.doc.files[index]
        let folded = model.folded.contains(index)
        return VStack(alignment: .leading, spacing: 0) {
            section(index, file: file, folded: folded)
        }
    }

    @ViewBuilder
    private func section(_ index: Int, file: ReviewFile, folded: Bool) -> some View {
        FileHeading(
            label: model.label(ofFile: index), path: file.path, added: file.added,
            removed: file.removed, comments: model.count(inFile: index), folded: folded,
            toggle: { model.toggle(file: index) }, list: { listing = true }
        )
        .id(Self.fileId(index))
        .identified("review.file.\(index)", label: file.path, value: folded ? "folded" : "open")
        if !folded {
            ForEach(file.comments.indices, id: \.self) { comment in
                CommentThread(text: file.comments[comment])
            }
            if file.binary {
                Text("Binary file")
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
                    .padding(.horizontal, design.metrics.gutter)
                    .padding(.vertical, 8)
            }
            ForEach(file.hunks.indices, id: \.self) { hunk in
                Text(verbatim: file.hunks[hunk].header)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .lineLimit(1)
                    .padding(.horizontal, design.metrics.gutter)
                    .padding(.vertical, 6)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(design.raised.color.opacity(0.5))
                ForEach(file.hunks[hunk].lines.indices, id: \.self) { line in
                    let at = ReviewLine(file: index, hunk: hunk, line: line)
                    let diff = file.hunks[hunk].lines[line]
                    DiffLineRow(line: diff, selected: model.isSelected(at))
                        .background(GeometryReader { geometry in
                            Color.clear.preference(
                                key: LineFrames.self,
                                value: [at: geometry.frame(in: .named(Self.space))])
                        })
                        .id(at)
                        .accessibilityAction(named: Text("Select line")) { select(at) }
                        .identified(
                            "review.line.\(index).\(hunk).\(line)", label: diff.text,
                            value: model.isSelected(at) ? "selected" : nil)
                    ForEach(diff.comments.indices, id: \.self) { comment in
                        CommentThread(text: diff.comments[comment])
                    }
                }
            }
        }
    }

    /// Selecting a line without a finger to hold it: VoiceOver's action on
    /// a line starts a selection, and on another line of the same file
    /// extends it there.
    private func select(_ at: ReviewLine) {
        if model.selection == nil || model.anchor?.file != at.file { model.begin(at: at) } else { model.extend(to: at) }
    }

    /// Hold a line for a moment, then drag: the selection follows the
    /// finger. A quick swipe fails the hold and scrolls instead.
    private var selecting: some Gesture {
        LongPressGesture(minimumDuration: 0.3)
            .sequenced(before: DragGesture(minimumDistance: 0, coordinateSpace: .named(Self.space)))
            .onChanged { value in
                guard case .second(true, let drag?) = value else { return }
                guard let line = line(at: drag.location) else { return }
                if model.anchor == nil || drag.translation == .zero {
                    if model.anchor == nil || !(model.selection?.contains(line) ?? false) {
                        model.begin(at: line)
                    }
                    return
                }
                model.extend(to: line)
            }
    }

    private func line(at point: CGPoint) -> ReviewLine? {
        frames.first { $0.value.minY <= point.y && point.y < $0.value.maxY }?.key
    }

    // MARK: - The foot

    @ViewBuilder
    private var footer: some View {
        if let selection = model.selection {
            HStack(spacing: 10) {
                Button { model.clearSelection() } label: {
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
                    Text(ChatWords.commentOn(lines: lines(in: selection)))
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ground.color)
                        .padding(.horizontal, 18)
                        .frame(height: 44)
                        .background(Capsule().fill(design.ink.color))
                }
                .buttonStyle(.amuxControl)
                .identified("review.comment", label: ChatWords.commentOn(lines: lines(in: selection)))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frosted(Capsule(), as: .glass)
            .padding(.horizontal, design.metrics.gutter)
            .padding(.bottom, 8)
        } else if model.canAttach {
            Button { actions(.attach) } label: {
                Text(ChatWords.attachReview(model.count))
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ground.color)
                    .frame(maxWidth: .infinity)
                    .frame(height: 50)
                    .background(Capsule().fill(design.ink.color))
            }
            .buttonStyle(.amuxControl)
            .identified("review.attach", label: ChatWords.attachReview(model.count))
            .padding(.horizontal, design.metrics.gutter)
            .padding(.bottom, 8)
        }
    }

    private func lines(in selection: ClosedRange<ReviewLine>) -> Int {
        guard selection.lowerBound.file == selection.upperBound.file else { return 1 }
        let file = model.doc.files[selection.lowerBound.file]
        var count = 0
        for hunk in file.hunks.indices {
            for line in file.hunks[hunk].lines.indices
            where selection.contains(ReviewLine(file: selection.lowerBound.file, hunk: hunk, line: line)) {
                count += 1
            }
        }
        return max(count, 1)
    }

    // MARK: - Writing a comment

    private func commentSheet(_ proxy: ScrollViewProxy) -> some View {
        ZStack(alignment: .bottom) {
            Color.black.opacity(0.25)
                .ignoresSafeArea()
                .onTapGesture { writing = false }
                .accessibilityHidden(true)
            CommentSheet(
                title: model.selection.map { ChatWords.commentOn(lines: lines(in: $0)) } ?? "",
                note: $note,
                cancel: { writing = false },
                add: {
                    if model.comment(note) {
                        note = ""
                        writing = false
                    }
                })
        }
        .onAppear {
            // The lines being written about stay in sight above the sheet.
            if let last = model.selection?.upperBound { proxy.scrollTo(last, anchor: .init(x: 0.5, y: 0.3)) }
        }
    }
}

/// The frame each drawn line has in the page, so a finger's position names
/// a line. Only drawn lines report, which are the only ones a finger is on.
private struct LineFrames: PreferenceKey {
    static let defaultValue: [ReviewLine: CGRect] = [:]
    static func reduce(value: inout [ReviewLine: CGRect], nextValue: () -> [ReviewLine: CGRect]) {
        value.merge(nextValue()) { $1 }
    }
}

/// "4 files · +18 −28", in the diff's own green and red.
struct Counts: View {
    @Environment(\.design) private var design
    let files: Int
    let added: UInt32
    let removed: UInt32

    var body: some View {
        HStack(spacing: 6) {
            Text(ChatWords.files(files))
                .foregroundStyle(design.inkMuted.color)
            Text(verbatim: "·").foregroundStyle(design.inkFaint.color)
            Text(verbatim: "+\(added)").foregroundStyle(design.added.color)
            Text(verbatim: "−\(removed)").foregroundStyle(design.removed.color)
        }
        .designFont(.monoSmall, design)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(ChatWords.changes(files: files, added: added, removed: removed))
    }
}

/// A file's heading: the fold, its name, the way to every file, how many
/// comments it has and what it changed.
private struct FileHeading: View {
    @Environment(\.design) private var design
    let label: String
    let path: String
    let added: UInt32
    let removed: UInt32
    let comments: Int
    let folded: Bool
    let toggle: () -> Void
    let list: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: toggle) {
                HStack(spacing: 8) {
                    Image(systemName: folded ? "chevron.right" : "chevron.down")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                        .frame(width: 14)
                    Text(verbatim: label)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(2)
                        .truncationMode(.head)
                        .layoutPriority(1)
                    if comments > 0 {
                        Label {
                            Text(verbatim: "\(comments)")
                        } icon: {
                            Image(systemName: "text.bubble")
                        }
                        .labelStyle(.titleAndIcon)
                        .designFont(.caption, design)
                        .foregroundStyle(design.accent.color)
                        .accessibilityLabel(ChatWords.comments(comments))
                    }
                    Spacer(minLength: 4)
                    Text(verbatim: "+\(added)").foregroundStyle(design.added.color)
                        .designFont(.monoSmall, design)
                        .fixedSize()
                    Text(verbatim: "−\(removed)").foregroundStyle(design.removed.color)
                        .designFont(.monoSmall, design)
                        .fixedSize()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel(path)
            .accessibilityValue(folded ? String(localized: "Folded") : String(localized: "Open"))
            Button(action: list) {
                Text("All Files")
                    .designFont(.caption, design)
                    .foregroundStyle(design.accent.color)
                    .fixedSize()
                    .thumbTarget(x: 6, y: 12)
            }
            .buttonStyle(.amuxControl)
            .identified("review.files", label: String(localized: "All Files"))
            .reclaimingThumbTarget(x: 6, y: 12)
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.trailing, 14)
        .padding(.vertical, 10)
        .background(design.ground.color)
        .overlay(alignment: .bottom) { Rectangle().fill(design.hairline.color).frame(height: 0.5) }
    }
}

/// One line of the patch: its old and new numbers, and its text on the
/// diff's green or red.
private struct DiffLineRow: View {
    @Environment(\.design) private var design
    let line: DiffLine
    let selected: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            number(line.oldLine)
            number(line.newLine)
            Text(verbatim: mark + line.text)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.ink.color)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.leading, 8)
        .padding(.trailing, 26)
        .padding(.vertical, 1)
        .background(tint)
        .overlay(alignment: .leading) {
            if selected { Rectangle().fill(design.accent.color).frame(width: 3) }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(ChatWords.spoken(line))
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var mark: String {
        switch line.kind {
        case .added: "+"
        case .removed: "−"
        case .context: " "
        }
    }

    private var tint: Color {
        if selected { return design.accent.color.opacity(0.18) }
        return switch line.kind {
        case .added: design.added.color.opacity(0.12)
        case .removed: design.removed.color.opacity(0.12)
        case .context: line.comments.isEmpty ? .clear : design.sunken.color
        }
    }

    private func number(_ value: UInt32?) -> some View {
        Text(verbatim: value.map(String.init) ?? "")
            .designFont(.monoSmall, design)
            .foregroundStyle(design.inkFaint.color)
            .frame(width: 30, alignment: .trailing)
            .accessibilityHidden(true)
    }
}

/// A comment under the line it ends on, with the accent bar that says it is
/// the reader's own.
private struct CommentThread: View {
    @Environment(\.design) private var design
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Rectangle().fill(design.accent.color).frame(width: 3)
            Text(verbatim: text)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.vertical, 8)
        }
        .padding(.leading, design.metrics.gutter)
        .padding(.trailing, 26)
        .background(design.raised.color)
        .identified("review.comment.thread", label: text)
    }
}

/// The comment being written, in the page over a scrim so the lines it is
/// about stay in sight.
private struct CommentSheet: View {
    @Environment(\.design) private var design
    let title: String
    @Binding var note: String
    let cancel: () -> Void
    let add: () -> Void
    @FocusState private var focused: Bool

    private var blank: Bool { note.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(verbatim: title)
                .designFont(.caption, design)
                .foregroundStyle(design.inkMuted.color)
            TextField(String(localized: "Comment"), text: $note, axis: .vertical)
                .lineLimit(2...6)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .focused($focused)
                .identified("review.note", label: String(localized: "Comment"), value: note)
            HStack {
                Button(action: cancel) {
                    Text("Cancel")
                        .designFont(.body, design)
                        .foregroundStyle(design.inkMuted.color)
                        .frame(height: 44)
                }
                .buttonStyle(.amuxControl)
                .identified("review.cancel", label: String(localized: "Cancel"))
                Spacer()
                Button(action: add) {
                    Text("Add to Review")
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ground.color)
                        .padding(.horizontal, 18)
                        .frame(height: 44)
                        .background(Capsule().fill(design.ink.color))
                }
                .buttonStyle(.amuxControl)
                .disabled(blank)
                .identified("review.add", label: String(localized: "Add to Review"), enabled: !blank)
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .padding(.horizontal, 10)
        .padding(.bottom, 8)
        .onAppear { focused = true }
    }
}

/// Every file, to jump to one: path, comments and what it changed.
public struct ReviewFileList: View {
    @Environment(\.design) private var design
    let model: ReviewModel
    let open: (Int) -> Void

    public init(model: ReviewModel, open: @escaping (Int) -> Void) {
        self.model = model
        self.open = open
    }

    public var body: some View {
        NavigationStack {
            List(model.doc.files.indices, id: \.self) { index in
                let file = model.doc.files[index]
                Button { open(index) } label: {
                    HStack(spacing: 8) {
                        Text(verbatim: file.path)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.ink.color)
                            .lineLimit(2)
                            .truncationMode(.head)
                        Spacer(minLength: 4)
                        if model.count(inFile: index) > 0 {
                            Text(verbatim: "\(model.count(inFile: index))")
                                .designFont(.caption, design)
                                .foregroundStyle(design.accent.color)
                                .accessibilityLabel(ChatWords.comments(model.count(inFile: index)))
                        }
                        Text(verbatim: "+\(file.added)").foregroundStyle(design.added.color)
                            .designFont(.monoSmall, design)
                        Text(verbatim: "−\(file.removed)").foregroundStyle(design.removed.color)
                            .designFont(.monoSmall, design)
                    }
                }
                .identified("review.list.\(index)", label: file.path)
            }
            .navigationTitle(String(localized: "Files"))
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}

/// A bar per file down the right edge, as tall as its share of the change,
/// with a dot where the reader has commented. Dragging it jumps between
/// files and names the one under the finger. VoiceOver reads the headings
/// and the file list instead.
private struct EdgeWheel: View {
    @Environment(\.design) private var design
    let model: ReviewModel
    let jump: (Int) -> Void
    @State private var touched: Int?

    var body: some View {
        GeometryReader { geometry in
            let spans = spans(in: geometry.size.height)
            ZStack(alignment: .topTrailing) {
                ForEach(spans.indices, id: \.self) { index in
                    let span = spans[index]
                    RoundedRectangle(cornerRadius: 1.5)
                        .fill(touched == index ? design.accent.color : design.inkFaint.color.opacity(0.5))
                        .frame(width: 3, height: max(span.height - 3, 2))
                        .offset(x: -8, y: span.minY)
                    if model.count(inFile: index) > 0 {
                        Circle().fill(design.accent.color)
                            .frame(width: 5, height: 5)
                            .offset(x: -15, y: span.minY)
                    }
                }
                if let touched, spans.indices.contains(touched) {
                    Text(verbatim: model.label(ofFile: touched))
                        .designFont(.caption, design)
                        .foregroundStyle(design.ink.color)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .frosted(Capsule(), as: .glass)
                        .fixedSize()
                        .offset(x: -28, y: max(spans[touched].midY - 12, 0))
                }
            }
            .frame(width: geometry.size.width, height: geometry.size.height, alignment: .topTrailing)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        guard let index = spans.firstIndex(where: { $0.maxY > value.location.y }) ?? spans.indices.last
                        else { return }
                        if touched != index { jump(index) }
                        touched = index
                    }
                    .onEnded { _ in touched = nil })
        }
        .frame(width: 22)
        .accessibilityHidden(true)
    }

    /// Each file's stretch of the edge, sized by lines changed with a floor
    /// so a one-line file can still be touched.
    private func spans(in height: CGFloat) -> [CGRect] {
        let files = model.doc.files
        guard !files.isEmpty, height > 0 else { return [] }
        let weights = files.map { CGFloat(max($0.added + $0.removed, 1)) }
        let floor: CGFloat = min(12, height / CGFloat(files.count))
        let spare = max(height - floor * CGFloat(files.count), 0)
        let total = weights.reduce(0, +)
        var y: CGFloat = 0
        return weights.map { weight in
            let span = floor + spare * weight / total
            defer { y += span }
            return CGRect(x: 0, y: y, width: 22, height: span)
        }
    }
}
