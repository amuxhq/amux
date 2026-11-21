import AmuxCore
import AmuxDesign
import AmuxWords
import SwiftUI

// The review page's rows, each drawn from one of the shared page's rows as
// handed: the page and a sent review's round draw the same ones.

/// A file's head: the fold, its path, what it changed and how many comments
/// it has, and under the path what a reader may want to know before opening
/// it. Pinned at the top while its file is read; a tap folds or opens it.
struct ReviewFileHeadRow: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    let head: ReviewFileHead
    var selected = false
    /// Its counts are drawn: a round lists files only for their comments.
    var tally = true
    /// Whether it changed since the last round is said: on all changes,
    /// not on the view that lists only those.
    var marksSince = true
    var toggle: (() -> Void)?

    var body: some View {
        let label = HStack(alignment: .firstTextBaseline, spacing: 8) {
            if toggle != nil {
                Image(systemName: head.folded ? "chevron.right" : "chevron.down")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
                    .frame(width: 14)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: head.path)
                    .designFont(.identifier, design)
                    .foregroundStyle(head.inChange || !tally ? design.ink.color : design.inkMuted.color)
                    .lineLimit(typeSize.isAccessibilitySize ? 4 : 2)
                    .truncationMode(.head)
                if let about = about {
                    Text(verbatim: about)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(2)
                }
            }
            .layoutPriority(1)
            Spacer(minLength: 4)
            if head.comments > 0 {
                Label {
                    Text(verbatim: "\(head.comments)")
                } icon: {
                    Image(systemName: "text.bubble")
                }
                .labelStyle(.titleAndIcon)
                .designFont(.caption, design)
                .foregroundStyle(design.accent.color)
                .accessibilityLabel(ChatWords.comments(Int(head.comments)))
            }
            if tally {
                Delta(added: head.added, removed: head.removed)
                    .designFont(.monoSmall, design)
            }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .contentShape(Rectangle())
        Group {
            if let toggle {
                Button(action: toggle) { label }.buttonStyle(.amuxPush)
            } else {
                label
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel([head.path, about].compactMap { $0 }.joined(separator: ", "))
        .accessibilityValue(toggle == nil ? "" : (head.folded ? String(localized: "Folded") : String(localized: "Open")))
        .accessibilityAddTraits(selected ? .isSelected : [])
        .background(selected ? design.accent.color.opacity(0.18) : design.ground.color)
        .overlay(alignment: .leading) {
            if selected { Rectangle().fill(design.accent.color).frame(width: 3) }
        }
        .overlay(alignment: .bottom) { Rectangle().fill(design.hairline.color).frame(height: 0.5) }
    }

    /// "Renamed from old.rs · Generated · Changed since your last review",
    /// or that the file is listed only for its comments.
    private var about: String? {
        var parts: [String] = []
        if let old = head.oldPath { parts.append(ChatWords.reviewRenamedFrom(old)) }
        parts += head.flags.map(ChatWords.reviewFlag)
        if head.binary { parts.append(String(localized: "Binary file")) }
        if !head.inChange && tally { parts.append(ChatWords.reviewOutsideChange) }
        if head.changedSinceLast && marksSince { parts.append(ChatWords.reviewChangedSinceLast) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// Where a hunk sits in the code: a sunken band with "⋯" in the number
/// column and the definitions its changes are in, outermost first.
struct ReviewHunkBand: View {
    @Environment(\.design) private var design
    @ScaledMetric(relativeTo: .footnote) private var numbers: CGFloat = 32
    @ScaledMetric(relativeTo: .footnote) private var marks: CGFloat = 12
    let hunk: ReviewHunkHead

    var body: some View {
        let scope = hunk.scope.isEmpty
            ? ChatWords.reviewLines(start: hunk.newStart, end: hunk.newStart)
            : hunk.scope.joined(separator: " \u{203A} ")
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(verbatim: "⋯")
                .frame(width: numbers, alignment: .trailing)
                .padding(.trailing, 8 + marks)
            Text(verbatim: scope)
                .lineLimit(1)
                .truncationMode(.head)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .designFont(.monoSmall, design)
        .foregroundStyle(design.inkFaint.color)
        .padding(.leading, 4)
        .padding(.trailing, 12)
        .padding(.vertical, 5)
        .background(design.sunken.color)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(scope)
        .identified("review.hunk", label: scope)
    }
}

/// One line of code: its number, its mark, and its text in the colours of
/// its tokens on the change's green or red, with the words an edited line
/// changed marked more strongly. A removed line is numbered as a line of
/// the commit compared against, every other as the snapshot's. A long line
/// wraps under its own text, not under the mark.
struct ReviewCodeLineRow: View {
    /// The number column fits four digits and the mark one sign, at the
    /// size the code is drawn.
    @ScaledMetric(relativeTo: .footnote) private var numbers: CGFloat = 32
    @ScaledMetric(relativeTo: .footnote) private var marks: CGFloat = 12

    @Environment(\.design) private var design
    let line: ReviewCodeLine
    var selected = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(verbatim: (line.newLine ?? line.oldLine).map(String.init) ?? "")
                .foregroundStyle(line.commented ? design.accent.color : design.inkFaint.color)
                .lineLimit(1)
                .frame(width: numbers, alignment: .trailing)
                .padding(.trailing, 8)
            Text(verbatim: mark)
                .foregroundStyle(markColor)
                .frame(width: marks, alignment: .leading)
            Text(words)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .designFont(.monoSmall, design)
        .padding(.leading, 4)
        .padding(.trailing, 12)
        .padding(.vertical, 1)
        .background(tint)
        .overlay(alignment: .leading) {
            if selected {
                Rectangle().fill(design.accent.color).frame(width: 3)
            } else if line.commented {
                Rectangle().fill(design.accent.color.opacity(0.6)).frame(width: 2)
            }
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spoken)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    /// The runs in their token colours; a changed run sits on a stronger
    /// tint of its side's colour.
    private var words: AttributedString {
        guard !line.runs.isEmpty else { return AttributedString(line.text.isEmpty ? " " : line.text) }
        var words = AttributedString()
        for run in line.runs {
            var piece = AttributedString(run.text)
            piece.foregroundColor = design.ink(run.kind).color
            if run.changed {
                piece.backgroundColor = side.opacity(0.30)
            }
            words += piece
        }
        return words
    }

    private var side: Color {
        line.kind == .removed ? design.removed.color : design.added.color
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
        case .context: line.extra ? design.sunken.color.opacity(0.5) : .clear
        }
    }

    /// "Line 41, added: let a = 2;"
    private var spoken: String {
        let number = (line.newLine ?? line.oldLine).map { ChatWords.reviewLines(start: $0, end: $0) }
        let kind: String? = switch line.kind {
        case .added: String(localized: "added")
        case .removed: String(localized: "removed")
        case .context: nil
        }
        let head = [number, kind].compactMap { $0 }.joined(separator: ", ")
        return head.isEmpty ? line.text : "\(head): \(line.text)"
    }
}

/// Lines of the file not shown, and the asks that load more of them: up to
/// the definition the code below starts in, or a step, and down likewise.
struct ReviewGapRow: View {
    @Environment(\.design) private var design
    @ScaledMetric(relativeTo: .footnote) private var numbers: CGFloat = 32
    let gap: ReviewGap
    let load: (ReviewContextAsk) -> Void

    var body: some View {
        HStack(spacing: 6) {
            Text(verbatim: "⋯")
                .designFont(.monoSmall, design)
                .frame(width: numbers, alignment: .trailing)
                .padding(.trailing, 4)
            if gap.loading {
                StateMark(state: .working)
                Text("Loading…")
                    .designFont(.caption, design)
            } else {
                Text(verbatim: ChatWords.reviewGap(gap))
                    .designFont(.caption, design)
                    .lineLimit(2)
                    .layoutPriority(1)
            }
            Spacer(minLength: 4)
            if !gap.loading {
                if let up = gap.up { ask(up, up: true) }
                if let down = gap.down { ask(down, up: false) }
            }
        }
        .foregroundStyle(design.inkMuted.color)
        .padding(.leading, 4)
        .padding(.trailing, 8)
        .frame(minHeight: 44)
        .background(design.sunken.color)
        .identified("review.gap", label: ChatWords.reviewGap(gap), value: gap.loading ? "loading" : nil)
    }

    private func ask(_ ask: ReviewContextAsk, up: Bool) -> some View {
        let words = ChatWords.reviewMore(ask, up: up)
        return Button { load(ask) } label: {
            Image(systemName: up ? "arrow.up.to.line" : "arrow.down.to.line")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(design.ink.color)
                .frame(width: 36, height: 30)
                .background(Capsule().fill(design.raised.color))
                .frame(width: 44, height: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel(words)
        .identified(up ? "review.gap.up" : "review.gap.down", label: words)
    }
}

/// A comment where the page placed it: under its lines, under its file's
/// head, or folded when outdated. A draft's bar is the accent, a sent one's
/// muted. One not drawn under its lines shows the code it was written on,
/// as stored with it.
struct ReviewCommentCard: View {
    @Environment(\.design) private var design
    let comment: ReviewCommentRow

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Rectangle()
                .fill(comment.state == .draft ? design.accent.color : design.inkMuted.color)
                .frame(width: 3)
            VStack(alignment: .leading, spacing: 6) {
                Text(verbatim: heading)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .lineLimit(comment.folded ? 1 : 2)
                if comment.folded {
                    Text(verbatim: comment.text)
                        .designFont(.body, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                } else {
                    if !comment.snippet.isEmpty {
                        VStack(alignment: .leading, spacing: 0) {
                            if !comment.scope.isEmpty {
                                Text(verbatim: comment.scope.joined(separator: " \u{203A} "))
                                    .designFont(.monoSmall, design)
                                    .foregroundStyle(design.inkFaint.color)
                                    .lineLimit(1)
                                    .truncationMode(.head)
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 3)
                            }
                            ForEach(Array(comment.snippet.enumerated()), id: \.offset) { _, line in
                                ReviewCodeLineRow(line: line)
                            }
                        }
                        .background(design.ground.color)
                        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                    }
                    Text(verbatim: comment.text)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .fixedSize(horizontal: false, vertical: true)
                    if comment.changedNear {
                        Label(ChatWords.reviewChangedNear, systemImage: "exclamationmark.circle")
                            .designFont(.caption, design)
                            .foregroundStyle(design.accent.color)
                    }
                }
            }
            .padding(.vertical, 8)
            if comment.folded {
                Image(systemName: "chevron.down")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
                    .padding(.top, 10)
            }
        }
        .padding(.leading, design.metrics.gutter)
        .padding(.trailing, 14)
        .background(design.raised.color)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    /// "Draft", "Sent in round 2 · Outdated", "Draft · On the file · Lines
    /// 41–42": its state, and where it sits when not under its lines.
    private var heading: String {
        var parts = [ChatWords.reviewCommentState(comment.state)]
        if let at = ChatWords.reviewCommentAt(comment.at) { parts.append(at) }
        if case .lines(let start, let end) = comment.at, !comment.snippet.isEmpty {
            parts.append(ChatWords.reviewLines(start: start, end: end))
        }
        return parts.joined(separator: " · ")
    }
}

/// What the page says about itself above the files, with what can be done
/// about it when anything: a refresh, or trying a snapshot again.
struct ReviewBannerCard: View {
    @Environment(\.design) private var design
    let banner: ReviewBanner
    var act: (() -> Void)?

    var body: some View {
        let words = ChatWords.reviewBanner(banner)
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: glyph)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(warns ? design.accent.color : design.inkMuted.color)
                    .accessibilityHidden(true)
                Text(verbatim: words)
                    .designFont(.detail, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let act, let action = ChatWords.reviewBannerAction(banner) {
                Button(action: act) { ActionLabel(action, kind: .outline) }
                    .buttonStyle(.amuxControl)
                    .identified("review.banner.action", label: action)
            }
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                .fill(design.raised.color))
        .identified("review.banner", label: words)
    }

    /// The banners that say the page is behind the code catch the eye.
    private var warns: Bool {
        switch banner {
        case .changedSinceYouStarted, .changedSince, .mergeUnderway, .snapshotFailed: true
        default: false
        }
    }

    private var glyph: String {
        switch banner {
        case .changedSinceYouStarted, .changedSince: "arrow.triangle.2.circlepath"
        case .movedDrafts: "arrow.right.doc.on.clipboard"
        case .mergeUnderway, .snapshotFailed: "exclamationmark.triangle"
        case .unreachable: "wifi.slash"
        case .notOffered, .notARepository, .roundGone: "info.circle"
        }
    }
}

/// A line saying the file's body is on its way, or why it has no lines.
struct ReviewNote: View {
    @Environment(\.design) private var design
    let text: String
    var loading = false

    var body: some View {
        HStack(spacing: 8) {
            if loading { StateMark(state: .working) }
            Text(verbatim: text)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A comment being written or edited, over a scrim so the lines it is about
/// stay in sight. Editing adds the way to delete it.
struct ReviewCommentSheet: View {
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
