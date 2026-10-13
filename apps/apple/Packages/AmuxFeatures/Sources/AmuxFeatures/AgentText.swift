import AmuxCore
import AmuxDesign
import SwiftUI

/// Words an agent wrote into something amux draws in its own place, such as
/// a question and its options, read as the inline markdown they are: a file
/// name between backticks is drawn as code, as it is in the chat, and the
/// backticks are not shown.
enum AgentText {
    /// What is drawn, without its markup: what VoiceOver says and what a
    /// driver reads.
    static func plain(_ text: String) -> String {
        String(MarkdownDocument.inline(text).characters)
    }
}

extension Design {
    /// Inline markdown with its code in the mono face and its links
    /// underlined, the way the chat draws an agent's prose.
    func inline(_ text: String) -> AttributedString {
        inline(MarkdownDocument.inline(text))
    }

    func inline(_ source: AttributedString) -> AttributedString {
        var text = source
        for run in text.runs {
            if run.inlinePresentationIntent == .code {
                text[run.range].font = font(.mono)
                text[run.range].foregroundColor = inkMuted.color
            }
            if run.link != nil {
                text[run.range].foregroundColor = accent.color
                text[run.range].underlineStyle = .single
            }
        }
        return text
    }
}
