import AmuxValues

extension ProsePart {
    /// The part as the segment it was parsed from: its words as written.
    public var segment: Segment {
        switch self {
        case .text(let markdown): .text(markdown.source)
        case .attachment(let view): .attachment(view)
        }
    }
}
