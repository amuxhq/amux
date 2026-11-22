import AmuxValues
import Foundation

/// An agent's prose as the Apple clients draw it.
///
/// Agents write markdown, so the transcript reads markdown: headings that are
/// headings, code that keeps its whitespace and its language, tables that line
/// up. The text arrives parsed, as the blocks every client draws
/// (`Markdown`, read by the shared view layer when it builds the row), so
/// nothing here parses: it turns a block's inline runs into attributed text
/// and says what a list's markers read.
public enum ProseText {
    /// Inline runs as attributed text: strong, emphasis, code and
    /// strikethrough as presentation intents, which `Text` draws and a
    /// design restyles.
    ///
    /// A link is underlined rather than coloured. Every colour in this design
    /// is either the neutral ramp or the one accent, and the accent means
    /// "something is waiting for you"; a link is not that, and the system's
    /// own blue is not a colour this app owns. A link this device will not
    /// open stays as its words.
    public static func attributed(_ runs: [InlineRun]) -> AttributedString {
        var text = AttributedString()
        for run in runs {
            var piece = AttributedString(run.text)
            var intent: InlinePresentationIntent = []
            if run.strong { intent.insert(.stronglyEmphasized) }
            if run.emphasis { intent.insert(.emphasized) }
            if run.code { intent.insert(.code) }
            if run.strikethrough { intent.insert(.strikethrough) }
            if !intent.isEmpty { piece.inlinePresentationIntent = intent }
            if let target = run.link, let link = OpenableLink(string: target) {
                piece.link = link.url
                piece.underlineStyle = .single
            }
            text.append(piece)
        }
        return text
    }

    /// The runs' words, without their markup: what VoiceOver says and what a
    /// driver reads.
    public static func plain(_ runs: [InlineRun]) -> String {
        runs.map(\.text).joined()
    }

    /// The marker each item of a list is drawn after: its number counted up
    /// from the list's first, a bullet, or for a task whether it is done.
    /// A client sets them in one column as wide as the widest, so every
    /// item's words start at one edge.
    public static func markers(ordered: Bool, start: UInt64, items: [ProseItem]) -> [String] {
        items.enumerated().map { index, item in
            switch item.task {
            case .some(true): "\u{2713}"
            case .some(false): "\u{25CB}"
            case .none: ordered ? "\(start + UInt64(index))." : "\u{2022}"
            }
        }
    }
}
