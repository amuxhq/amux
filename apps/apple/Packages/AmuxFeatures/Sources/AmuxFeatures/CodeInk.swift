import AmuxCore
import AmuxDesign

extension Design {
    /// The colour a kind of code token is drawn in: names and punctuation
    /// in the reading ink, and a block written as a diff in a change's
    /// green and red.
    ///
    /// Here rather than in the design package, which holds the colours and
    /// knows nothing of the bridge's values: what a token is comes from the
    /// shared view, and the screens that draw code are what pair the two.
    func ink(_ kind: TokenKind) -> Ramp {
        switch kind {
        case .plain: ink
        case .keyword: syntax.keyword
        case .string: syntax.string
        case .number: syntax.number
        case .type: syntax.type
        case .function: syntax.function
        case .comment: syntax.comment
        case .inserted: added
        case .deleted: removed
        }
    }
}
