import AmuxValues
import AmuxWords
import SwiftUI

/// The tool window's Review tab: the open agent's changes, to read and
/// comment on line by line. The tab and the toggle that closes it are on
/// the title row above.
///
/// Each time it shows it opens the agent's review for a look, and draws
/// that page while it is open. Where the agent's host offered no reviews
/// as the page opened, the review over a patch stands in.
public struct ChangesPane: View {
    let chat: any ChatSurface
    /// The review has been asked to open, so the page it gives decides
    /// which review is drawn.
    @State private var opened = false

    public init(chat: any ChatSurface) {
        self.chat = chat
    }

    public var body: some View {
        Group {
            if !opened {
                Color.clear
            } else if let page = chat.reviewPage {
                ReviewPagePane(review: page)
            } else {
                PatchReviewPane(chat: chat)
            }
        }
        .onAppear {
            chat.openReviewPage()
            opened = true
        }
        .onDisappear { chat.closeReviewPage() }
        // Another agent's chat is another review: the page shut here opens
        // there.
        .id(ObjectIdentifier(chat))
    }
}

/// The open agent's changes as its host freezes them in a patch, with what
/// is compared and its counts in the window's bar.
private struct PatchReviewPane: View {
    let chat: any ChatSurface

    var body: some View {
        VStack(spacing: 0) {
            WindowBar(leading: 8, rule: false) {
                comparison
                Spacer(minLength: 6)
                ChangeCounts(changes: chat.changes).font(.ui(.meta))
                BarButton(symbol: "refresh-cw", help: "Refresh", size: 14) {
                    chat.loadReview(keepingComments: false)
                }
            }
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
        let label = HStack(spacing: 4) {
            Text(Self.compared(chat.comparison, base: chat.base))
                .font(.ui(.body, weight: .medium))
                .lineLimit(1)
            if chat.comparisons.count > 1 {
                Icon("chevron-down", size: 11).foregroundStyle(Palette.textTertiary)
            }
        }
        .padding(.horizontal, 6)
        .frame(height: 28)
        if chat.comparisons.count > 1 {
            Menu {
                ForEach(chat.comparisons, id: \.self) { comparison in
                    Button {
                        chat.compare(comparison)
                    } label: {
                        if comparison == chat.comparison {
                            Label(Self.compared(comparison, base: chat.base), systemImage: "checkmark")
                        } else {
                            Text(Self.compared(comparison, base: chat.base))
                        }
                    }
                }
            } label: {
                label
            }
            .menuStyle(.button)
            .buttonStyle(HoverButtonStyle())
            .menuIndicator(.hidden)
            .fixedSize()
        } else {
            label
        }
    }

    /// What the diff compares, in a few words: "Uncommitted", "vs main".
    static func compared(_ comparison: Comparison, base: String?) -> String {
        switch comparison {
        case .uncommitted: "Uncommitted"
        case .onBranch: base.map { "vs \($0)" } ?? "vs base branch"
        }
    }

    @ViewBuilder private var content: some View {
        switch chat.review {
        case .idle, .loading:
            ProgressView().controlSize(.small)
        case .nothing:
            VStack(spacing: 8) {
                Icon("diff", size: 22).foregroundStyle(Palette.textTertiary)
                Text("No changes").foregroundStyle(Palette.textSecondary)
            }
        case .failed(let why):
            Text(why).foregroundStyle(Palette.textSecondary).multilineTextAlignment(.center).padding()
        case .ready(let review):
            ReviewView(review: review, attach: chat.attachReview)
        }
    }
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
                    Text(ChatWords.comments(review.count)).foregroundStyle(Palette.textSecondary)
                    Spacer()
                    Button(ChatWords.attachReview(review.count), action: attach)
                        .buttonStyle(PillButtonStyle(prominent: true, small: true))
                }
                .font(.ui(.meta))
                .padding(10)
            }
        }
    }

    @ViewBuilder private func hunks(_ file: ReviewFile, _ index: Int) -> some View {
        ForEach(file.comments, id: \.self) { comment in Comment(text: comment) }
        if file.binary {
            Text("Binary file").font(.ui(.meta)).foregroundStyle(Palette.textTertiary).padding(12)
        }
        ForEach(Array(file.hunks.enumerated()), id: \.offset) { hunkIndex, hunk in
            if hunk.skips || !hunk.scope.isEmpty {
                Text(hunk.scope.isEmpty ? "⋯" : hunk.scope)
                    .font(.mono(.caption))
                    .foregroundStyle(Palette.textTertiary)
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
                    .foregroundStyle(Palette.textTertiary)
                    .frame(width: 12)
                FileName(path: file.path, from: file.oldPath)
                Spacer(minLength: 6)
                if file.status == .added { Text("new").foregroundStyle(Palette.added) }
                if file.status == .deleted { Text("deleted").foregroundStyle(Palette.removed) }
                // Only the sides that moved: "+9" for a new file reads
                // cleaner than "+9 −0", and a binary file has no lines.
                if file.added > 0 { Text("+\(file.added)").foregroundStyle(Palette.added).monospacedDigit() }
                if file.removed > 0 { Text("−\(file.removed)").foregroundStyle(Palette.removed).monospacedDigit() }
            }
            .font(.ui(.meta))
            .padding(.horizontal, 12)
            .frame(height: 34)
            .background(Palette.content)
            .overlay(alignment: .bottom) { Rectangle().fill(Palette.line).frame(height: 1) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// A file's name in the text colour after its folder in a quieter one, so
/// the eye finds the file; its old path first when it moved.
struct FileName: View {
    let path: String
    let from: String?

    var body: some View {
        let folder = (path as NSString).deletingLastPathComponent
        let name = (path as NSString).lastPathComponent
        (Text(from.map { "\($0) → " } ?? "").foregroundStyle(Palette.textTertiary)
            + Text(folder.isEmpty ? "" : folder + "/").foregroundStyle(Palette.textTertiary)
            + Text(name).foregroundStyle(.primary).fontWeight(.medium))
            .font(.ui(.meta))
            .lineLimit(1)
            .truncationMode(.head)
    }
}

/// One line of the diff, with its numbers, picked for a comment by a click.
private struct DiffLineView: View {
    let line: DiffLine
    let selected: Bool
    @State private var hovering = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(line.oldLine.map(String.init) ?? "")
                .frame(width: 32, alignment: .trailing)
                .foregroundStyle(Palette.textTertiary)
            Text(line.newLine.map(String.init) ?? "")
                .frame(width: 32, alignment: .trailing)
                .foregroundStyle(Palette.textTertiary)
                .padding(.trailing, 6)
            PatchText(kind: line.kind, text: line.text)
        }
        .font(.mono(.small))
        // The numbers take the line's tint too, so an added or removed
        // line reads as one band.
        .background(PatchText(kind: line.kind, text: "").tint)
        // A bar down the edge marks the changed lines as a column, so a
        // change reads at a glance even where its tint is faint.
        .overlay(alignment: .leading) {
            switch line.kind {
            case .added: Rectangle().fill(Palette.added).frame(width: 2)
            case .removed: Rectangle().fill(Palette.removed).frame(width: 2)
            case .context: EmptyView()
            }
        }
        .overlay(selected ? Palette.accent.opacity(0.14) : (hovering ? Palette.hover : .clear))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .help("Click to comment. Shift-click to select more lines.")
    }
}

private struct Comment: View {
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Icon("message-square-text", size: 14).foregroundStyle(Palette.accent)
            Text(text).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .font(.ui(.meta))
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).strokeBorder(Palette.line))
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
            TextField("Add a comment", text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...6)
                .focused($focused)
                .onSubmit(save)
            HStack {
                Button("Cancel", action: cancel)
                    .buttonStyle(PillButtonStyle(small: true))
                    .keyboardShortcut(.escape, modifiers: [])
                Button("Comment", action: save)
                    .buttonStyle(PillButtonStyle(prominent: true, small: true))
                    .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .font(.ui(.body))
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.card))
        .overlay(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).strokeBorder(Color.primary.opacity(0.16)))
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .onAppear { focused = true }
    }
}

