import AmuxCore
import AmuxValues
import AmuxDesign
import AmuxWords
import SwiftUI

extension Design {
    /// Words an agent wrote, as the inline runs the shared view layer read
    /// them into, with their code in the mono face and their links
    /// underlined, the way the chat draws an agent's prose. A question and
    /// its options come read this way too: a file name between backticks is
    /// drawn as code and the backticks are not shown.
    func inline(_ runs: [InlineRun]) -> AttributedString {
        var text = ProseText.attributed(runs)
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
