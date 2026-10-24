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
/// heading that stays pinned while its file is read and that folds it.
/// Holding a line for a moment and dragging selects lines; a comment goes
/// on the line the selection ends on and is drawn under it, and tapping it
/// edits or deletes it. The edge wheel down the right jumps between files,
/// and the bar's file menu lists them. Attaching puts the whole review into
/// the chat's draft as one token that sends with the reference to the patch
/// it was written against.
public struct ReviewPage: View {
    @Environment(\.design) private var design
    @Environment(\.reducesTransparency) private var reducesTransparency
    let model: ReviewModel
    /// What the changes are counted against, as the overview words it:
    /// "Uncommitted", "vs main".
    let against: String?
    let actions: (ReviewAction) -> Void
    @State private var writing = false
    /// The comment being edited, as its index in the model's comments.
    @State private var editing: Int?
    @State private var note = ""
    @State private var target: Int?
    @State private var frames: [ReviewLine: CGRect] = [:]
    /// Files with lines on screen, so the wheel can mark the one being read.
    @State private var visible: Set<Int> = []

    public init(model: ReviewModel, against: String? = nil, writing: Bool = false,
                editing: Int? = nil, actions: @escaping (ReviewAction) -> Void) {
        self.model = model
        self.against = against
        self.actions = actions
        _writing = State(initialValue: writing)
        _editing = State(initialValue: editing)
        _note = State(initialValue: editing.flatMap { model.comments.indices.contains($0) ? model.comments[$0].text : nil } ?? "")
    }

    public var body: some View {
        ScrollViewReader { proxy in
            ZStack(alignment: .bottom) {
                Ground()
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                        ForEach(model.doc.files.indices, id: \.self) { index in
                            Section {
                                file(index)
                            } header: {
                                heading(index)
                            }
                        }
                    }
                    .padding(.bottom, 96)
                    .coordinateSpace(.named(Self.space))
                    .onPreferenceChange(LineFrames.self) { frames = $0 }
                    .gesture(selecting)
                }
                .scrollDismissesKeyboard(.interactively)
                .overlay(alignment: .trailing) {
                    EdgeWheel(model: model, current: visible.min()) {
                        proxy.scrollTo(Self.fileId($0), anchor: .top)
                    }
                    // Clear of the pinned heading above and the page's
                    // button below.
                    .padding(.top, 52)
                    .padding(.bottom, model.selection != nil || model.canAttach ? 76 : 12)
                }
                footer
                if writing || editing != nil { commentSheet(proxy) }
            }
            .onChange(of: target) { _, file in
                guard let file else { return }
                proxy.scrollTo(Self.fileId(file), anchor: .top)
                target = nil
            }
        }
        .navigationBarBackButtonHidden(true)
        .toolbarTitleDisplayMode(.inline)
        .toolbar { bar }
        .accessibilityElement(children: .contain)
        .reported("review", value: "\(model.count)")
    }

    static let space = "review"
    static func fileId(_ index: Int) -> String { "review.file.\(index)" }

    // MARK: - The bar

    /// The system's bar as the chat draws it: the way back in a circle,
    /// what the page is in a pill beside it, and the file menu at the
    /// trailing edge.
    @ToolbarContentBuilder
    private var bar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            BarBack(identifier: "review.back") { actions(.back) }
        }
        .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
        ToolbarSpacer(.fixed, placement: .topBarLeading)
        ToolbarItem(placement: .topBarLeading) {
            title
        }
        .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
        ToolbarItem(placement: .topBarTrailing) {
            files
        }
        .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
    }

    /// "Review" over "+7 −4 · Uncommitted"; the files are counted in the
    /// menu beside it. Tapping it goes back
    /// to the first file, as tapping a bar goes back to the top.
    private var title: some View {
        Button {
            target = 0
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                Text("Review")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                HStack(spacing: Space.xs) {
                    Delta(added: model.doc.added, removed: model.doc.removed)
                    if let against {
                        Text(verbatim: "·").foregroundStyle(design.inkFaint.color)
                        Text(verbatim: against)
                            .foregroundStyle(design.inkMuted.color)
                            .identified("review.against", label: against)
                    }
                }
                .designFont(.caption, design)
                .monospacedDigit()
                .lineLimit(1)
            }
            // The bar does not grow with the text: past the largest ordinary
            // size the pill would no longer fit it.
            .dynamicTypeSize(...DynamicTypeSize.xLarge)
            .padding(.horizontal, Space.m)
            .frame(maxWidth: 220, alignment: .leading)
            .frame(height: 36)
            .barItemSurface()
        }
        .accessibilityLabel(ChatWords.changes(
            files: model.doc.files.count, added: model.doc.added, removed: model.doc.removed))
        .accessibilityHint("Goes to the first file")
        .identified("review.summary", label: ChatWords.changes(
            files: model.doc.files.count, added: model.doc.added, removed: model.doc.removed))
    }

    /// Every file, to jump to one: its name, what it changed and how many
    /// comments it has. The one being read is ticked.
    private var files: some View {
        MenuButton(
            name: String(localized: "Files"), identifier: "review.files",
            items: model.doc.files.indices.map { index in
                MenuItem(
                    title: model.label(ofFile: index), subtitle: summary(ofFile: index),
                    chosen: index == visible.min()
                ) { target = index }
            }
        ) {
            Image(systemName: "list.bullet")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(design.ink.color)
                .frame(width: 36, height: 36)
                .barItemSurface()
        }
    }

    /// "+3 −4 · 1 comment", leaving out what is nothing.
    private func summary(ofFile index: Int) -> String {
        let file = model.doc.files[index]
        var parts: [String] = []
        let delta = [file.added > 0 ? "+\(file.added)" : nil, file.removed > 0 ? "\u{2212}\(file.removed)" : nil]
            .compactMap { $0 }.joined(separator: " ")
        if !delta.isEmpty { parts.append(delta) }
        if file.binary { parts.append(String(localized: "Binary file")) }
        let comments = model.count(inFile: index)
        if comments > 0 { parts.append(ChatWords.comments(comments)) }
        return parts.joined(separator: " · ")
    }

    // MARK: - A file

    private func heading(_ index: Int) -> some View {
        let file = model.doc.files[index]
        let folded = model.folded.contains(index)
        return FileHeading(
            label: model.label(ofFile: index), path: file.path, added: file.added,
            removed: file.removed, comments: model.count(inFile: index), folded: folded,
            toggle: { model.toggle(file: index) }
        )
        .id(Self.fileId(index))
        .identified("review.file.\(index)", label: file.path, value: folded ? "folded" : "open")
    }

    /// One file's lines under its heading, unless folded. The list is lazy
    /// by file, not by line.
    @ViewBuilder
    private func file(_ index: Int) -> some View {
        let file = model.doc.files[index]
        if !model.folded.contains(index) {
            VStack(alignment: .leading, spacing: 0) {
                threads(CommentPlace(file: index, line: nil))
                if file.binary {
                    Text("Binary file")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        .padding(.horizontal, design.metrics.gutter)
                        .padding(.vertical, 8)
                }
                ForEach(file.hunks.indices, id: \.self) { hunk in
                    if file.hunks[hunk].skips { HunkBreak(scope: file.hunks[hunk].scope) }
                    ForEach(file.hunks[hunk].lines.indices, id: \.self) { line in
                        self.line(ReviewLine(file: index, hunk: hunk, line: line))
                    }
                }
            }
            .onScrollVisibilityChange(threshold: 0.01) { shown in
                if shown { visible.insert(index) } else { visible.remove(index) }
            }
        }
    }

    @ViewBuilder
    private func line(_ at: ReviewLine) -> some View {
        let diff = model.doc.files[at.file].hunks[at.hunk].lines[at.line]
        DiffLineRow(line: diff, selected: model.isSelected(at))
            .background(GeometryReader { geometry in
                Color.clear.preference(
                    key: LineFrames.self,
                    value: [at: geometry.frame(in: .named(Self.space))])
            })
            .id(at)
            .accessibilityAction(named: Text("Select line")) { select(at) }
            .identified(
                "review.line.\(at.file).\(at.hunk).\(at.line)", label: diff.text,
                value: model.isSelected(at) ? "selected" : nil)
        if !diff.comments.isEmpty { threads(CommentPlace(file: at.file, line: at)) }
    }

    /// The comments drawn at one place; tapping one edits it.
    private func threads(_ place: CommentPlace) -> some View {
        ForEach(model.placed[place] ?? [], id: \.self) { index in
            Button {
                note = model.comments[index].text
                editing = index
            } label: {
                CommentThread(text: model.comments[index].text)
            }
            .buttonStyle(.amuxPush)
            .accessibilityHint("Edits the comment")
            .identified("review.comment.thread", label: model.comments[index].text)
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
                .onTapGesture { close() }
                .accessibilityHidden(true)
            if let editing {
                CommentSheet(
                    title: String(localized: "Edit Comment"), action: String(localized: "Save"),
                    actionId: "review.save", note: $note, cancel: close,
                    add: { if model.rewrite(editing, as: note) { close() } },
                    remove: {
                        model.remove(editing)
                        close()
                    })
            } else {
                CommentSheet(
                    title: model.selection.map { ChatWords.commentOn(lines: lines(in: $0)) } ?? "",
                    action: String(localized: "Add to Review"), actionId: "review.add",
                    note: $note, cancel: close,
                    add: { if model.comment(note) { close() } })
            }
        }
        .onAppear {
            // The lines being written about stay in sight above the sheet.
            if let editing, let line = place(of: editing) {
                proxy.scrollTo(line, anchor: .init(x: 0.5, y: 0.3))
            } else if let last = model.selection?.upperBound {
                proxy.scrollTo(last, anchor: .init(x: 0.5, y: 0.3))
            }
        }
    }

    private func close() {
        note = ""
        writing = false
        editing = nil
    }

    /// The line a comment is drawn under, if it is on a line.
    private func place(of comment: Int) -> ReviewLine? {
        model.placed.first { $0.value.contains(comment) }?.key.line
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

/// A file's heading: the fold, its name, how many comments it has and what
/// it changed. Pinned at the top while its file is read.
private struct FileHeading: View {
    @Environment(\.design) private var design
    let label: String
    let path: String
    let added: UInt32
    let removed: UInt32
    let comments: Int
    let folded: Bool
    let toggle: () -> Void

    var body: some View {
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
                Delta(added: added, removed: removed)
                    .designFont(.monoSmall, design)
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.vertical, 10)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxPush)
        .accessibilityLabel(path)
        .accessibilityValue(folded ? String(localized: "Folded") : String(localized: "Open"))
        .background(design.ground.color)
        .overlay(alignment: .bottom) { Rectangle().fill(design.hairline.color).frame(height: 0.5) }
    }
}

/// One line of the patch: its number, its mark and its text on the diff's
/// green or red. A removed line is numbered on the old side, every other
/// on the new. A long line wraps under its own text, not under the mark.
private struct DiffLineRow: View {
    @Environment(\.design) private var design
    let line: DiffLine
    let selected: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(verbatim: (line.newLine ?? line.oldLine).map(String.init) ?? "")
                .foregroundStyle(design.inkFaint.color)
                .frame(width: 32, alignment: .trailing)
                .padding(.trailing, 8)
                .accessibilityHidden(true)
            Text(verbatim: mark)
                .foregroundStyle(markColor)
                .frame(width: 12, alignment: .leading)
            Text(verbatim: line.text)
                .foregroundStyle(design.ink.color)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .designFont(.monoSmall, design)
        .padding(.leading, 4)
        .padding(.trailing, 22)
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
        case .removed: "\u{2212}"
        case .context: ""
        }
    }

    private var markColor: Color {
        switch line.kind {
        case .added: design.added.color
        case .removed: design.removed.color
        case .context: .clear
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
}

/// Lines the patch leaves out before a hunk: a sunken band with "⋯" in the
/// number column and git's name for where the hunk is, as the terminal's
/// break does.
private struct HunkBreak: View {
    @Environment(\.design) private var design
    let scope: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(verbatim: "⋯")
                .frame(width: 32, alignment: .trailing)
                .padding(.trailing, 8 + 12)
            Text(verbatim: scope)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .designFont(.monoSmall, design)
        .foregroundStyle(design.inkFaint.color)
        .padding(.leading, 4)
        .padding(.trailing, 22)
        .padding(.vertical, 4)
        .background(design.sunken.color)
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
        .padding(.trailing, 22)
        .background(design.raised.color)
        .contentShape(Rectangle())
    }
}

/// A comment being written or edited, in the page over a scrim so the lines
/// it is about stay in sight. Editing adds the way to delete it.
private struct CommentSheet: View {
    @Environment(\.design) private var design
    let title: String
    let action: String
    let actionId: String
    @Binding var note: String
    let cancel: () -> Void
    let add: () -> Void
    var remove: (() -> Void)?
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
            HStack(spacing: 16) {
                if let remove {
                    Button(action: remove) {
                        Text("Delete")
                            .designFont(.body, design)
                            .foregroundStyle(design.removed.color)
                            .frame(height: 44)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("review.delete", label: String(localized: "Delete"))
                    Spacer()
                }
                Button(action: cancel) {
                    Text("Cancel")
                        .designFont(.body, design)
                        .foregroundStyle(design.inkMuted.color)
                        .frame(height: 44)
                }
                .buttonStyle(.amuxControl)
                .identified("review.cancel", label: String(localized: "Cancel"))
                if remove == nil { Spacer() }
                Button(action: add) {
                    Text(verbatim: action)
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ground.color)
                        .padding(.horizontal, 18)
                        .frame(height: 44)
                        .background(Capsule().fill(design.ink.color))
                }
                .buttonStyle(.amuxControl)
                .disabled(blank)
                .identified(actionId, label: action, enabled: !blank)
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .padding(.horizontal, 10)
        .padding(.bottom, 8)
        .onAppear { focused = true }
    }
}

/// A bar per file down the right edge, as tall as its share of the change.
/// The file being read is drawn darker and a file with comments in the
/// accent. Dragging it jumps between files and names the one under the
/// finger. VoiceOver reads the headings and the file menu instead.
private struct EdgeWheel: View {
    @Environment(\.design) private var design
    let model: ReviewModel
    let current: Int?
    let jump: (Int) -> Void
    @State private var touched: Int?

    var body: some View {
        GeometryReader { geometry in
            let spans = spans(in: geometry.size.height)
            ZStack(alignment: .topTrailing) {
                ForEach(spans.indices, id: \.self) { index in
                    let span = spans[index]
                    Capsule()
                        .fill(color(index))
                        .frame(width: touched == index ? 4 : 2, height: max(span.height - 4, 2))
                        .offset(x: -8, y: span.minY)
                }
                if let touched, spans.indices.contains(touched) {
                    HStack(spacing: 8) {
                        Text(verbatim: model.label(ofFile: touched))
                            .designFont(.caption, design)
                            .foregroundStyle(design.ink.color)
                        Delta(added: model.doc.files[touched].added, removed: model.doc.files[touched].removed)
                            .designFont(.caption, design)
                            .monospacedDigit()
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .frosted(Capsule(), as: .glass)
                    .fixedSize()
                    .offset(x: -24, y: max(spans[touched].midY - 14, 0))
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

    private func color(_ index: Int) -> Color {
        let commented = model.count(inFile: index) > 0
        let lit = touched == index || (touched == nil && current == index)
        if commented { return design.accent.color.opacity(lit ? 1 : 0.55) }
        return lit ? design.inkMuted.color : design.inkFaint.color.opacity(0.4)
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
