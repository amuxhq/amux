import AmuxCore
import Foundation

/// A chat with no runtime behind it: rows, a frame, a card and what
/// surrounds them set by whoever builds it. The component catalogue draws the real chat views
/// from one, and a test drives a model through it. What the script changes
/// is gathered as the runtime gathers it, and handed to a model as one
/// delta by `deliver(to:)`.
public final class ScriptedChat: ChatSource, @unchecked Sendable {
    private let lock = NSLock()
    private var ordered: [Row]
    private var current: ChatFrame
    private var card: AskCard?
    private var facts: Overview
    /// The changed files the overview lists once it asks for them.
    private var files: Changes?
    private var offered: SettingsView?
    private var images: [[UInt8]: Data]
    private var pending = Pending()
    /// What was sent, in order, for a test to read back.
    public private(set) var sent: [Draft] = []
    /// The settings picks sent, in order.
    public private(set) var changed: [SettingChange] = []
    /// The most rows the window holds while the reader follows: past it the
    /// oldest go as new ones arrive, and the take names the oldest key
    /// left, which is how the session says the window moved.
    public var cap = Int.max
    /// What asking for older rows comes to.
    public var paged: PageOutcome = .arrived(0)
    /// How long asking for older rows takes to answer.
    public var pageTakes: Duration = .zero
    /// What asking for the working-tree diff comes to; nil answers that the
    /// machine could not be asked.
    public var working: FrozenReview?

    public init(
        rows: [Row], frame: ChatFrame, card: AskCard? = nil,
        strip: Surroundings = ScriptedChat.strip(), settings: SettingsView? = nil,
        images: [[UInt8]: Data] = [:]
    ) {
        ordered = rows
        current = strip.applied(to: frame)
        self.card = card
        facts = strip.overview
        files = strip.changes
        offered = settings
        self.images = images
    }

    // MARK: - Building values

    public static let agent = AgentKey(
        host: Array(repeating: 1, count: 16), agent: Array(repeating: 2, count: 16))

    public static func frame(
        name: String = "refactor-auth", kind: Kind = .claudeSdk, phase: PhaseView = .idle,
        mode: Composer = .send, activity: Activity? = nil, caughtUp: Bool = true,
        hasOlder: Bool = false, queue: [QueuedRow] = [], underway: [SentPrompt] = [],
        refused: [RefusedPrompt] = [], waiting: Waiting? = nil, switchedAway: Bool = false
    ) -> ChatFrame {
        ChatFrame(
            agent: agent, name: name, kind: kind, phase: phase,
            composer: ComposerView(mode: mode, switchedAway: switchedAway, activity: activity), controls: ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil), connection: .live,
            caughtUp: caughtUp, hasOlder: hasOlder, arrivalsHeld: false, queue: queue,
            underway: underway, refused: refused, askInput: nil, context: nil,
            ended: nil,
            git: nil, signIn: nil, waiting: waiting)
    }

    /// The facts around a chat's rows: what the frame reports of the agent,
    /// and the overview.
    public struct Surroundings: Sendable {
        public var overview: Overview
        public var controls: ControlsSummary
        public var context: ContextView?
        public var signIn: SignInView?
        public var git: Git?
        /// What the overview lists once it asks for the changed files.
        public var changes: Changes?

        /// The frame reporting these facts.
        func applied(to frame: ChatFrame) -> ChatFrame {
            var frame = frame
            frame.controls = controls
            frame.context = context
            frame.signIn = signIn
            frame.git = git
            return frame
        }
    }

    public static func strip(
        tasks: TasksView? = nil, context: ContextView? = nil, model: String? = nil,
        effort: String? = nil, mode: String? = nil, usage: UsageView? = nil,
        failedServers: [ServerView] = [], signIn: SignInView? = nil, background: UInt32? = nil,
        git: Git? = nil, changes: Changes? = nil
    ) -> Surroundings {
        let jobs = (0..<(background ?? 0)).map {
            JobRow(command: "job \($0 + 1)", startedAtMs: 0, kind: .shell, step: nil)
        }
        return Surroundings(
            overview: Overview(
                jobs: jobs, failedServers: failedServers, changes: nil, tasks: tasks,
                usageNearLimit: usage),
            controls: ControlsSummary(
                effort: effort, mode: nil, model: model, permission: mode.flatMap(Self.unusual)),
            context: context, signIn: signIn, git: git, changes: changes)
    }

    /// A permission by its provider value as the controls summary carries
    /// it: only one that is not the provider's normal one.
    static func unusual(_ value: String) -> PermissionChoice? {
        let names = [
            "acceptEdits": "Accept edits", "plan": "Plan", "auto": "Auto",
            "bypassPermissions": "Bypass permissions", "read-only": "Read only",
            "full-access": "Full access",
        ]
        guard let name = names[value] else { return nil }
        return PermissionChoice(
            value: value, displayName: name, custom: false, current: true, unlisted: false,
            normal: false, neverAsks: value == "bypassPermissions" || value == "full-access",
            settable: true)
    }

    public static func row(
        _ id: String, _ order: UInt64, _ kind: RowKind, attention: Bool = false,
        decision: Decision? = nil, run: Run? = nil, parent: String? = nil,
        collapsed: Bool = false
    ) -> Row {
        Row(
            id: id, order: order, atMs: 1_790_000_000_000 + Int64(order) * 1_000, kind: kind,
            collapsed: collapsed || parent != nil, attention: attention, decision: decision,
            parent: parent, run: run)
    }

    // MARK: - Changing it

    /// What moved since the last delta.
    private struct Pending {
        var above: [String] = []
        var below: [String] = []
        var dropped = 0
        var rows: [String] = []
        var session = false

        /// Keys arriving at the newest edge, whose rows are read when drawn.
        mutating func arrived(_ keys: [String]) {
            above.append(contentsOf: keys)
        }

        /// The oldest key went as the window trimmed: one the model never
        /// heard of is simply not handed over.
        mutating func dropped(_ key: String) {
            if let at = above.firstIndex(of: key) {
                above.remove(at: at)
            } else if let at = below.firstIndex(of: key) {
                below.remove(at: at)
            } else {
                dropped += 1
            }
        }
    }

    /// Rows arriving at the newest edge, as the session takes them: while
    /// the reader follows they join the rows and the next delta hands them
    /// over; in history they are held apart until the reader returns, and
    /// the frame says they are.
    public func append(_ rows: [Row]) {
        lock.withLock {
            if following {
                ordered.append(contentsOf: rows)
                pending.arrived(rows.map(\.id))
                trim()
            } else {
                held.append(contentsOf: rows)
                current.arrivalsHeld = true
                pending.session = true
            }
        }
    }

    private func trim() {
        while ordered.count > cap { pending.dropped(ordered.removeFirst().id) }
    }

    /// Puts rows before the oldest, as a page of older history lands, for
    /// the next delta to hand over.
    public func prepend(_ rows: [Row]) {
        lock.withLock {
            ordered.insert(contentsOf: rows, at: 0)
            pending.below.insert(contentsOf: rows.map(\.id), at: 0)
        }
    }

    /// Replaces rows in place, for the next delta to hand over.
    public func revise(_ rows: [Row]) {
        lock.withLock {
            for row in rows {
                if let index = ordered.firstIndex(where: { $0.id == row.id }) { ordered[index] = row }
            }
            pending.rows.append(contentsOf: rows.map(\.id))
        }
    }

    /// What moved since the last delta, as the runtime hands it over; nil
    /// when nothing did.
    public func delta() -> ChatDelta? {
        lock.withLock {
            defer { pending = Pending() }
            let keys: KeysMoved? =
                pending.above.isEmpty && pending.below.isEmpty && pending.dropped == 0
                ? nil
                : .edges(above: pending.above, below: pending.below, dropped: UInt32(pending.dropped))
            let wanted = Set(pending.rows).subtracting(pending.below).subtracting(pending.above)
            let rows = ordered.filter { wanted.contains($0.id) }.map { Self.folding($0, open: openRuns) }
            guard keys != nil || !rows.isEmpty || pending.session else { return nil }
            let session = pending.session
            return ChatDelta(
                chat: 0, rows: rows, ask: session ? AskMoved(card: card) : nil,
                frame: session ? current : nil, keys: keys, overview: session ? facts : nil,
                settings: session ? offered : nil)
        }
    }

    /// Hands a model what moved since the last delta, if anything did.
    @MainActor
    public func deliver(to model: ChatModel) {
        if let delta = delta() { model.apply(delta) }
    }

    public func show(
        frame: ChatFrame? = nil, card: AskCard?? = nil, strip: Surroundings? = nil,
        settings: SettingsView? = nil
    ) {
        lock.withLock {
            if let frame { current = frame }
            if let card { self.card = card }
            if let strip {
                facts = strip.overview
                files = strip.changes
                current = strip.applied(to: current)
            }
            if let settings { offered = settings }
            pending.session = true
        }
    }

    // MARK: - ChatSource

    public func opening() -> ChatDelta {
        lock.withLock {
            pending = Pending()
            return ChatDelta(
                chat: 0, rows: [], ask: AskMoved(card: card), frame: current,
                keys: .reload(ordered.map(\.id)), overview: facts, settings: offered)
        }
    }

    /// Where the reader was last said to be, in order, for a test to read.
    public private(set) var follows: [Bool] = []
    private var following = true
    private var held: [Row] = []
    /// The runs the reader opened, by their newest step.
    private var openRuns: Set<String> = []

    public func follow(_ following: Bool) {
        lock.withLock {
            follows.append(following)
            self.following = following
            guard following, !held.isEmpty else { return }
            ordered.append(contentsOf: held)
            pending.arrived(held.map(\.id))
            trim()
            held = []
            current.arrivalsHeld = false
            pending.session = true
        }
    }

    public func rows(for keys: [String]) -> [Row] {
        lock.withLock {
            let wanted = Set(keys)
            return ordered.filter { wanted.contains($0.id) }.map { Self.folding($0, open: openRuns) }
        }
    }

    /// A row as the shared fold leaves it: a finished run shows its newest
    /// step and the failures its turn left unfixed, a live run its newest
    /// few, and an open run every step.
    static func folding(_ row: Row, open: Set<String>) -> Row {
        guard var run = row.run else { return row }
        let shown = run.live
            ? run.recent.map { $0 < 3 } ?? false
            : run.recent == 0 || run.unresolvedFailure
        run.open = open.contains(run.last)
        var row = row
        row.run = run
        row.collapsed = row.collapsed || (!shown && !run.open)
        return row
    }

    /// Rows of one run of tool steps, with the run's fields filled in as
    /// the shared fold fills them: every row carries the run, only its
    /// newest three steps say how many follow, and only the newest carries
    /// what the steps did. `failed` names steps the turn left unfixed.
    public static func run(
        _ rows: [Row], live: Bool = false, openBelow: Bool = false, failed: Set<String> = []
    ) -> [Row] {
        let steps = rows.filter { !$0.kind.passesThroughRuns }
        guard let first = steps.first, let last = steps.last else { return rows }
        var counts = RunCounts(commands: 0, edits: 0, reads: 0, searches: 0, subagents: 0, other: 0)
        for step in steps {
            switch step.kind {
            case .command, .background: counts.commands += 1
            case .fileChange(let files, _): counts.edits += UInt32(max(files.count, 1))
            case .explore(.read, _, _, _): counts.reads += 1
            case .explore(.search, _, _, _), .explore(.webSearch, _, _, _): counts.searches += 1
            case .subagent: counts.subagents += 1
            default: counts.other += 1
            }
        }
        let total = UInt32(steps.count)
        return rows.map { row in
            var row = row
            let index = steps.firstIndex { $0.id == row.id }
            let after = index.map { total - 1 - UInt32($0) }
            row.run = Run(
                first: first.id, last: last.id, steps: total, live: live, openBelow: openBelow,
                unresolvedFailure: failed.contains(row.id), open: false,
                counts: row.id == last.id ? counts : nil,
                recent: after.flatMap { $0 < 3 ? $0 : nil })
            return row
        }
    }

    public func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        lock.withLock { sent.append(draft) }
        return .success(SendOutcome(inputId: [1], state: .sent))
    }

    public func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { .done }
    public func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { .done }
    public func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { .done }
    /// Opens or closes a run by its newest step, for the next delta to hand
    /// its rows over; a script's runs do not grow.
    public func toggleRun(_ member: String) {
        lock.withLock {
            guard let last = ordered.first(where: { $0.id == member })?.run?.last else { return }
            if openRuns.remove(last) == nil { openRuns.insert(last) }
            pending.rows.append(contentsOf: ordered.filter { $0.run?.last == last }.map(\.id))
        }
    }
    public func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? { .done }
    public func withdraw(_ input: [UInt8]) async -> ActOutcome? { .done }
    public func sendNow(_ input: [UInt8]) async -> ActOutcome? { .done }
    public func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    /// Forgets a prompt on its way or refused, as the session does.
    public func discard(_ input: [UInt8]) {
        lock.withLock {
            current.underway.removeAll { $0.inputId == input }
            current.refused.removeAll { $0.inputId == input }
            pending.session = true
        }
    }

    /// The words of a refused prompt, by its id: what a session keeps of
    /// what it sent.
    public var refusedWords: [[UInt8]: String] = [:]

    /// The words of the queued, underway or refused prompt with this id; a
    /// scripted chat holds no attachments behind its rows.
    public func draft(of input: [UInt8]) -> Draft? {
        let frame = lock.withLock { current }
        if let words = refusedWords[input] { return Draft(words: words, attachments: []) }
        let text = frame.queue.first { $0.inputId == input }?.text
            ?? frame.underway.first { $0.inputId == input }?.text
        return text.map { segments in
            let words = segments.compactMap { segment -> String? in
                if case .text(let text) = segment { text } else { nil }
            }
            return Draft(words: words.joined(), attachments: [])
        }
    }
    public func interrupt() async -> ActOutcome? { .done }

    public func change(_ setting: SettingChange) async -> ActOutcome? {
        lock.withLock { changed.append(setting) }
        return .done
    }
    public func resume(with draft: Draft) async -> ActOutcome? { .done }
    public func pageOlder(_ rows: UInt32) async -> PageOutcome? {
        let takes = lock.withLock { pageTakes }
        if takes > .zero { try? await Task.sleep(for: takes) }
        return lock.withLock { paged }
    }

    public func putBlob(
        _ data: Data, name: String, mime: String
    ) async -> Result<BlobRef, RuntimeFailure> {
        let blob = BlobRef(hash: Array(name.utf8), name: name, mime: mime, size: UInt64(data.count))
        lock.withLock { images[blob.hash] = data }
        return .success(blob)
    }

    public func blob(_ hash: [UInt8]) -> Data? { lock.withLock { images[hash] } }

    public func review(_ comparison: Comparison) async -> Result<FrozenReview, RuntimeFailure> {
        guard let working = lock.withLock({ working }) else {
            return .failure(RuntimeFailure("the machine could not be asked"))
        }
        return .success(working)
    }

    public func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> {
        .success(lock.withLock {
            facts.changes = files
            return facts
        })
    }
}
