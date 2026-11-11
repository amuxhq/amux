import AmuxValues

/// How a grid row joins the rail: whether the next row drawn below it is on
/// the rail too, and whether it is a subagent's step, set in under its
/// parent's text.
public struct RailJoin: Equatable, Sendable {
    public var continues = false
    public var nested = false

    public init(continues: Bool = false, nested: Bool = false) {
        self.continues = continues
        self.nested = nested
    }

    public static let none = RailJoin()

    /// The join for `row` with `next` drawn below it.
    public static func of(_ row: Row, next: Row?) -> RailJoin {
        guard onRail(row) else { return .none }
        return RailJoin(continues: next.map(onRail) ?? false, nested: row.parent != nil)
    }

    /// Everything drawn on the grid hangs off the rail; what is read rather
    /// than scanned — the prompt, the prose — and the rules and footers that
    /// close a turn break it.
    public static func onRail(_ row: Row) -> Bool {
        switch row.kind {
        case .prompt, .prose, .turnEnd, .compaction, .boundary, .hidden: false
        default: true
        }
    }
}

/// Where a row stands in its run of tool steps, placed as the terminal
/// places it from the same shared fold.
public enum RunPlacement: Equatable, Sendable {
    /// Outside a run, or a step of an opened run with no line above it:
    /// drawn as itself.
    case own
    /// A step drawn as itself under its run's line: the opened run's first
    /// step, or the oldest of a live run's newest steps.
    case step(header: Header)
    /// A finished run's newest step, which draws as the run's one line.
    case folded(words: String)
    /// A step a folded run keeps in view: a failure its turn never fixed.
    case failed

    public struct Header: Equatable, Sendable {
        public let words: String
        public let open: Bool
    }

    /// The placement of a row the chat draws: `open` is whether its run is
    /// open, `counts` what the run's steps did.
    public static func of(_ row: Row, open: Bool, counts: () -> RunCounts?) -> RunPlacement {
        guard let run = row.run else { return .own }
        if open {
            // Opened, a run lists every step on its own line, under the
            // line that folds it again.
            guard row.id == run.first else { return .own }
            return .step(header: Header(words: ChatWords.run(run, counts: counts()), open: true))
        }
        if run.live {
            // Under way: the newest few steps, with how many came before.
            let shown = min(run.steps, liveSteps)
            let earlier = run.steps - shown
            guard earlier > 0, run.recent == shown - 1 else { return .own }
            return .step(header: Header(words: ChatWords.earlier(earlier), open: false))
        }
        if run.recent == 0 { return .folded(words: ChatWords.run(run, counts: counts())) }
        return .failed
    }

    /// While a run is under way, how many of its newest steps show: the
    /// shared fold's own number.
    public static let liveSteps: UInt32 = 3
}
