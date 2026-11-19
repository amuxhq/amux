import AmuxValues
import Foundation
import SwiftUI
import Synchronization

/// How an Apple client gets code coloured: a fenced block in an agent's
/// markdown, and the patch an edit card shows. The colouring is the shared
/// view's, so a block reads with the same runs here as in the terminal;
/// each client maps a ``TokenKind`` to its own palette.
///
/// A screen reads it from the environment (`codeHighlighting`). The app
/// puts in the bridge's (`CodeHighlighting.bridge`, in `AmuxCore`); a
/// screen drawn without the bridge, such as the Mac's storybook, draws its
/// code plain.
///
/// An answer can come back unsettled: a grammar is being built in the
/// background on its first use, or a block still streaming has lines not
/// yet passed over. It is drawn as it is and asked for again shortly
/// (``follow(_:_:)``). Settled answers are kept by content, so a block
/// scrolled back into view draws coloured at once, without crossing the
/// bridge again.
public struct CodeHighlighting: Sendable, Equatable {
    /// What colours are asked for.
    public enum Request: Hashable, Sendable {
        /// A fenced block: its code, the fence's info string, and whether
        /// it is the block still being written (the open fence of a
        /// message still streaming). Its completed lines are coloured at a
        /// bounded rate while it streams, so the cost does not grow with
        /// the square of its length.
        case block(code: String, info: String, streaming: Bool)
        /// An edit card's patch, coloured as the file at `path` reads.
        case edit(path: String, lines: [EditLine])
    }

    /// How long to wait before asking again for an unsettled answer: under
    /// the shared view's tenth of a second between passes over a streaming
    /// block, so a due pass starts and a finished one shows without
    /// waiting for more text to arrive.
    public static let again = Duration.milliseconds(50)

    private let answer: (@Sendable (Request) -> HighlightedCode?)?
    private let kept: Kept?

    /// Colours from `answer`, which may be called on any thread; nil from
    /// it draws the code plain.
    public init(_ answer: @escaping @Sendable (Request) -> HighlightedCode?) {
        self.answer = answer
        kept = Kept()
    }

    private init() {
        answer = nil
        kept = nil
    }

    /// Nothing coloured.
    public static let plain = CodeHighlighting()

    /// Two are the same when they share their keeping: the environment
    /// compares them, and a value carried into a hosted cell must compare
    /// equal to itself.
    public static func == (lhs: CodeHighlighting, rhs: CodeHighlighting) -> Bool {
        lhs.kept === rhs.kept
    }

    /// The settled answer kept for `request`, without asking.
    public func kept(_ request: Request) -> HighlightedCode? {
        kept?.held.withLock { $0[request] }
    }

    /// Asks for `request`'s colours on the calling thread, keeping a
    /// settled answer. Nil when nothing colours code here.
    public func ask(_ request: Request) -> HighlightedCode? {
        guard let answer, let kept else { return nil }
        if let held = self.kept(request) { return held }
        guard let code = answer(request) else { return nil }
        if code.settled { kept.keep(code, for: request) }
        return code
    }

    /// Asks for `request`'s colours away from the main thread and hands
    /// each answer to `draw`, asking again shortly while it is unsettled.
    /// Returns once an answer is settled or the task is cancelled, as a
    /// view's task is when what it draws changes. An answer already kept
    /// is not handed over: the view draws it from ``kept(_:)``.
    @MainActor
    public func follow(_ request: Request, _ draw: (HighlightedCode) -> Void) async {
        guard answer != nil, kept(request) == nil else { return }
        while !Task.isCancelled {
            let code = await Task.detached(priority: .userInitiated) { ask(request) }.value
            guard let code, !Task.isCancelled else { return }
            draw(code)
            if code.settled { return }
            try? await Task.sleep(for: Self.again)
        }
    }

    /// Settled answers by what they answer. Started again when full, as
    /// the shared view's own keeping is: what is on screen is asked for
    /// again within a frame, and nothing here is worth an eviction order.
    private final class Kept: Sendable {
        static let limit = 256
        let held = Mutex<[Request: HighlightedCode]>([:])

        func keep(_ code: HighlightedCode, for request: Request) {
            held.withLock { held in
                if held.count >= Self.limit { held.removeAll(keepingCapacity: true) }
                held[request] = code
            }
        }
    }
}

extension EnvironmentValues {
    /// How code on this screen is coloured; plain unless the app puts in
    /// the bridge's.
    @Entry public var codeHighlighting: CodeHighlighting = .plain
}

extension HighlightedCode {
    /// The runs to draw for `lines`, the code's lines as they are now: each
    /// line this answer coloured while its text is still that line's text,
    /// and any other line plain. So an answer for an earlier text of the
    /// same code goes on colouring what it still knows (a streaming block
    /// before its newest lines, a block that has just finished) while the
    /// answer for the new text is on its way, rather than the code
    /// flashing plain between them.
    public func runs(_ lines: [String]) -> [[CodeRun]] {
        lines.enumerated().map { index, line in
            if index < self.lines.count,
               self.lines[index].reduce(into: "", { $0 += $1.text }) == line {
                return self.lines[index]
            }
            return Self.plain(line)
        }
    }

    /// `lines` with no colour, when there is no answer to draw.
    public static func plain(_ lines: [String]) -> [[CodeRun]] {
        lines.map(plain)
    }

    private static func plain(_ line: String) -> [CodeRun] {
        line.isEmpty ? [] : [CodeRun(text: line, kind: .plain)]
    }
}

/// Code's runs as text to draw, each in its kind's colour from the
/// client's palette. A kind given no colour takes the text's own.
public enum CodeText {
    public static func attributed(
        _ runs: [CodeRun], _ colour: (TokenKind) -> Color?
    ) -> AttributedString {
        var text = AttributedString()
        for run in runs {
            var piece = AttributedString(run.text)
            if let colour = colour(run.kind) { piece.foregroundColor = colour }
            text += piece
        }
        return text
    }

    /// Lines of runs as one text, a line break between each.
    public static func attributed(
        lines: [[CodeRun]], _ colour: (TokenKind) -> Color?
    ) -> AttributedString {
        var text = AttributedString()
        for (index, line) in lines.enumerated() {
            if index > 0 { text += AttributedString("\n") }
            text += attributed(line, colour)
        }
        return text
    }

    /// A fenced block's code as the lines it is drawn in, one for each line
    /// break and one after the last. The shared view answers no line for an
    /// empty last one, which draws as nothing either way.
    public static func lines(_ code: String) -> [String] {
        code.components(separatedBy: "\n")
    }
}
