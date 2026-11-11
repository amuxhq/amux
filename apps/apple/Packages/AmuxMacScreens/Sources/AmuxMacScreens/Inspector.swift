import AmuxValues
import AmuxWords
import SwiftUI

/// The right-hand pane: the open agent's changes, to read and comment on
/// line by line, with what is compared and its counts in the pane's own
/// title bar.
public struct ChangesPane: View {
    let chat: any ChatSurface
    let close: @MainActor () -> Void
    @Environment(\.barHeight) private var bar

    public init(chat: any ChatSurface, close: @escaping @MainActor () -> Void = {}) {
        self.chat = chat
        self.close = close
    }

    public var body: some View {
        VStack(spacing: 0) {
            TopBar(height: bar) {
                comparison
                Spacer(minLength: 6)
                ChangeCounts(changes: chat.changes).font(.system(size: 12))
                BarButton(symbol: "refresh-cw", help: "Read the changes again", size: 12) {
                    chat.loadReview(keepingComments: false)
                }
                BarButton(symbol: "x", help: "Close (⌥⌘I)", size: 13, action: close)
            }
            Rectangle().fill(Palette.line).frame(height: 1)
            content.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        // New totals wait while comments are being written; the reload
        // button is there for when the person is ready.
        .task(id: Reload(comparison: chat.comparison, changes: chat.changes)) {
            chat.loadReview(keepingComments: true)
        }
    }

    private struct Reload: Equatable {
        let comparison: Comparison
        let changes: ChangeTotals?
    }

    /// "main → working tree", a menu where there is more than one way to
    /// compare.
    @ViewBuilder private var comparison: some View {
        let label = HStack(spacing: 5) {
            Icon("git-branch", size: 13).foregroundStyle(.secondary)
            Text(ChatWords.comparison(chat.comparison, base: chat.base))
                .font(.system(size: 13, weight: .medium))
                .lineLimit(1)
        }
        if chat.comparisons.count > 1 {
            Menu {
                ForEach(chat.comparisons, id: \.self) { comparison in
                    Button {
                        chat.compare(comparison)
                    } label: {
                        if comparison == chat.comparison {
                            Label(ChatWords.comparison(comparison, base: chat.base), systemImage: "checkmark")
                        } else {
                            Text(ChatWords.comparison(comparison, base: chat.base))
                        }
                    }
                }
            } label: {
                label
            }
            .menuStyle(.button)
            .buttonStyle(HoverButtonStyle())
            .fixedSize()
        } else {
            label
        }
    }

    @ViewBuilder private var content: some View {
        switch chat.review {
        case .idle, .loading:
            ProgressView().controlSize(.small)
        case .nothing:
            VStack(spacing: 8) {
                Icon("diff", size: 22).foregroundStyle(.tertiary)
                Text("No changes").foregroundStyle(.secondary)
            }
        case .failed(let why):
            Text(why).foregroundStyle(.secondary).multilineTextAlignment(.center).padding()
        case .ready(let review):
            ReviewView(review: review, attach: chat.attachReview)
        }
    }
}

extension EnvironmentValues {
    /// The window's title bar height, which every column's top bar takes.
    @Entry var barHeight: CGFloat = 38
}

/// The diff, a file at a time, with comments where they were written.
private struct ReviewView: View {
    let review: any ReviewSurface
    let attach: () -> Void
    @State private var writing = ""

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    ForEach(Array(review.doc.files.enumerated()), id: \.offset) { index, file in
                        Section {
                            // Its own stack, so the lines' identities are the
                            // file's alone and no two files' lines collide.
                            if !review.folded(index) {
                                VStack(alignment: .leading, spacing: 0) { hunks(file, index) }
                            }
                        } header: {
                            FileHeader(file: file, folded: review.folded(index)) { review.toggle(file: index) }
                        }
                    }
                }
            }
            if review.canAttach {
                Rectangle().fill(Palette.line).frame(height: 1)
                HStack {
                    Text(ChatWords.comments(review.count)).foregroundStyle(.secondary)
                    Spacer()
                    Button(ChatWords.attachReview(review.count), action: attach)
                        .buttonStyle(.borderedProminent)
                }
                .font(.system(size: 12))
                .padding(10)
            }
        }
    }

    @ViewBuilder private func hunks(_ file: ReviewFile, _ index: Int) -> some View {
        ForEach(file.comments, id: \.self) { comment in Comment(text: comment) }
        if file.binary {
            Text("Binary file").font(.system(size: 12)).foregroundStyle(.tertiary).padding(12)
        }
        ForEach(Array(file.hunks.enumerated()), id: \.offset) { hunkIndex, hunk in
            if hunk.skips || !hunk.scope.isEmpty {
                Text(hunk.scope.isEmpty ? "⋯" : hunk.scope)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.hover)
            }
            ForEach(Array(hunk.lines.enumerated()), id: \.offset) { lineIndex, line in
                let spot = ReviewSpot(file: index, hunk: hunkIndex, line: lineIndex)
                DiffLineView(line: line, selected: review.isSelected(spot))
                    .onTapGesture {
                        if NSEvent.modifierFlags.contains(.shift), review.selecting {
                            review.extend(to: spot)
                        } else {
                            writing = ""
                            review.begin(at: spot)
                        }
                    }
                ForEach(line.comments, id: \.self) { comment in Comment(text: comment) }
                if review.selecting, review.isSelected(spot), !review.isSelected(next(spot, in: hunk)) {
                    Writer(text: $writing) {
                        if review.comment(writing) { writing = "" }
                    } cancel: {
                        review.clearSelection()
                    }
                }
            }
        }
    }

    private func next(_ spot: ReviewSpot, in hunk: Hunk) -> ReviewSpot {
        ReviewSpot(file: spot.file, hunk: spot.hunk, line: spot.line + 1)
    }
}

private struct FileHeader: View {
    let file: ReviewFile
    let folded: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 6) {
                Icon(folded ? "chevron-right" : "chevron-down", size: 12)
                    .foregroundStyle(.tertiary)
                    .frame(width: 12)
                Text(file.oldPath.map { "\($0) → \(file.path)" } ?? file.path)
                    .font(.system(size: 12, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.head)
                Spacer(minLength: 6)
                if file.status == .added { Text("new").foregroundStyle(.green) }
                if file.status == .deleted { Text("deleted").foregroundStyle(.red) }
                Text("+\(file.added)").foregroundStyle(.green).monospacedDigit()
                Text("−\(file.removed)").foregroundStyle(.red).monospacedDigit()
            }
            .font(.system(size: 11.5))
            .padding(.horizontal, 10)
            .frame(height: 30)
            .background(Palette.content)
            .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 1) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// One line of the diff, with its numbers, picked for a comment by a click.
private struct DiffLineView: View {
    let line: DiffLine
    let selected: Bool
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 0) {
            Text(line.oldLine.map(String.init) ?? "")
                .frame(width: 34, alignment: .trailing)
                .foregroundStyle(.tertiary)
            Text(line.newLine.map(String.init) ?? "")
                .frame(width: 34, alignment: .trailing)
                .foregroundStyle(.tertiary)
                .padding(.trailing, 6)
            PatchText(kind: line.kind, text: line.text)
        }
        .font(.system(size: 11, design: .monospaced))
        .background(selected ? Color.accentColor.opacity(0.22) : (hovering ? Palette.hover : .clear))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .help("Click to comment on this line; Shift-click to take in more")
    }
}

private struct Comment: View {
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Icon("message-square-text", size: 14).foregroundStyle(Color.accentColor)
            Text(text).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .font(.system(size: 12))
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Palette.line))
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
    }
}

/// Where a comment on the picked lines is written.
private struct Writer: View {
    @Binding var text: String
    let save: () -> Void
    let cancel: () -> Void
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            TextField("Comment on these lines", text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...6)
                .focused($focused)
                .onSubmit(save)
            HStack {
                Button("Cancel", action: cancel).keyboardShortcut(.escape, modifiers: [])
                Button("Comment", action: save)
                    .buttonStyle(.borderedProminent)
                    .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .controlSize(.small)
        }
        .font(.system(size: 12))
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Color.accentColor.opacity(0.5)))
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .onAppear { focused = true }
    }
}

