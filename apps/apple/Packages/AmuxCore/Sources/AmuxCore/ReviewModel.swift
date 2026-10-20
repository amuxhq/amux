import AmuxApp
import AmuxValues
import Foundation
import Observation

/// Where a diff line sits in a review document: its file, its hunk, and its
/// place in that hunk. Ordered as the page draws them.
public struct ReviewLine: Hashable, Comparable, Sendable {
    public var file: Int
    public var hunk: Int
    public var line: Int

    public init(file: Int, hunk: Int, line: Int) {
        self.file = file
        self.hunk = hunk
        self.line = line
    }

    public static func < (a: ReviewLine, b: ReviewLine) -> Bool {
        (a.file, a.hunk, a.line) < (b.file, b.hunk, b.line)
    }
}

/// Where a comment is drawn: under one of a file's lines, or on the file
/// itself when `line` is nil.
public struct CommentPlace: Hashable, Sendable {
    public var file: Int
    public var line: ReviewLine?

    public init(file: Int, line: ReviewLine?) {
        self.file = file
        self.line = line
    }
}

/// One review page: an agent's working-tree diff frozen when the page
/// opened, the comments written on it, and the lines selected for the next.
///
/// The document is the runtime's: files, counts and hunks are parsed from
/// the frozen patch in Rust, and every comment added is placed on its line
/// by parsing again, so the page and the terminal's review agree on where a
/// comment sits.
@MainActor
@Observable
public final class ReviewModel {
    public let review: FrozenReview
    public private(set) var comments: [ReviewComment]
    public private(set) var doc: ReviewDoc
    /// Where each comment is drawn, as indexes into `comments` in the order
    /// the document places them.
    public private(set) var placed: [CommentPlace: [Int]] = [:]
    /// Files folded to their heading, by index.
    public private(set) var folded: Set<Int> = []
    /// The line a selection started on and the line it reaches, in one file.
    public private(set) var anchor: ReviewLine?
    public private(set) var head: ReviewLine?

    public init(review: FrozenReview, comments: [ReviewComment] = []) {
        self.review = review
        self.comments = comments
        doc = Self.document(review, comments: comments)
        placed = Self.places(comments, in: doc)
    }

    /// The runtime's document for a frozen diff with `comments` placed.
    public static func document(_ review: FrozenReview, comments: [ReviewComment]) -> ReviewDoc {
        let parsed = Bridge.json(review).withCString { review in
            Bridge.json(comments).withCString { comments in
                Bridge.read(ReviewDoc.self, amux_review_doc(review, comments))
            }
        }
        return parsed ?? ReviewDoc(base: "", head: review.diff.head, files: [], added: 0, removed: 0)
    }

    /// Where the runtime's document draws each comment: on the line it
    /// names (the new side's, or a removed line's old one), else on its file.
    static func places(_ comments: [ReviewComment], in doc: ReviewDoc) -> [CommentPlace: [Int]] {
        var placed: [CommentPlace: [Int]] = [:]
        for (index, comment) in comments.enumerated() {
            guard let file = doc.files.firstIndex(where: { $0.path == comment.path }) else { continue }
            var at: ReviewLine?
            if comment.line > 0 || comment.oldLine > 0 {
                search: for (hunk, lines) in doc.files[file].hunks.map(\.lines).enumerated() {
                    for (line, diff) in lines.enumerated() {
                        let hit = comment.line > 0
                            ? diff.newLine == comment.line
                            : diff.kind == .removed && diff.oldLine == comment.oldLine
                        if hit {
                            at = ReviewLine(file: file, hunk: hunk, line: line)
                            break search
                        }
                    }
                }
            }
            placed[CommentPlace(file: file, line: at), default: []].append(index)
        }
        return placed
    }

    // MARK: - Reading

    public func line(_ at: ReviewLine) -> DiffLine? {
        guard doc.files.indices.contains(at.file) else { return nil }
        let hunks = doc.files[at.file].hunks
        guard hunks.indices.contains(at.hunk), hunks[at.hunk].lines.indices.contains(at.line) else {
            return nil
        }
        return hunks[at.hunk].lines[at.line]
    }

    /// The selected lines, first to last.
    public var selection: ClosedRange<ReviewLine>? {
        guard let anchor, let head else { return nil }
        return min(anchor, head)...max(anchor, head)
    }

    public func isSelected(_ at: ReviewLine) -> Bool {
        selection?.contains(at) ?? false
    }

    /// Comments written so far, across every file.
    public var count: Int { comments.count }

    /// Comments on one file, on its lines or on the file itself.
    public func count(inFile index: Int) -> Int {
        guard doc.files.indices.contains(index) else { return 0 }
        let path = doc.files[index].path
        return comments.filter { $0.path == path }.count
    }

    /// What a heading calls a file: its name, or its folder and name where
    /// another changed file has the same name.
    public func label(ofFile index: Int) -> String {
        let path = doc.files[index].path
        let name = (path as NSString).lastPathComponent
        let shared = doc.files.indices.contains { other in
            other != index && (doc.files[other].path as NSString).lastPathComponent == name
        }
        guard shared else { return name }
        let parent = ((path as NSString).deletingLastPathComponent as NSString).lastPathComponent
        return parent.isEmpty ? name : "\(parent)/\(name)"
    }

    /// Whether the page can be attached: something written and nothing
    /// half-selected.
    public var canAttach: Bool { !comments.isEmpty && selection == nil }

    // MARK: - Acting

    public func toggle(file index: Int) {
        if folded.contains(index) { folded.remove(index) } else { folded.insert(index) }
    }

    /// Starts a selection on one line, as a held finger does.
    public func begin(at line: ReviewLine) {
        guard self.line(line) != nil else { return }
        anchor = line
        head = line
    }

    /// Extends the selection to a line the finger reached. A selection stays
    /// in the file it started in.
    public func extend(to line: ReviewLine) {
        guard let anchor, line.file == anchor.file, self.line(line) != nil else { return }
        head = line
    }

    public func clearSelection() {
        anchor = nil
        head = nil
    }

    /// Adds a comment on the line the selection ends on, and clears the
    /// selection. Blank text adds nothing.
    @discardableResult
    public func comment(_ text: String) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let last = selection?.upperBound, let line = line(last) else {
            return false
        }
        let (new, old): (UInt32, UInt32) = switch (line.newLine, line.oldLine) {
        case (let new?, _): (new, 0)
        case (nil, let old?): (0, old)
        case (nil, nil): (0, 0)
        }
        comments.append(ReviewComment(path: doc.files[last.file].path, line: new, oldLine: old, text: text))
        lay()
        clearSelection()
        return true
    }

    /// Places the comments again after they change.
    private func lay() {
        doc = Self.document(review, comments: comments)
        placed = Self.places(comments, in: doc)
    }

    /// Rewrites a comment where it stands. Blank text changes nothing.
    @discardableResult
    public func rewrite(_ index: Int, as text: String) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, comments.indices.contains(index) else { return false }
        comments[index].text = text
        lay()
        return true
    }

    public func remove(_ index: Int) {
        guard comments.indices.contains(index) else { return }
        comments.remove(at: index)
        lay()
    }

    /// The draft's token for this review: the frozen diff and every comment.
    public var attachment: DraftAttachment {
        .review(diff: review.diff, comments: comments)
    }
}
