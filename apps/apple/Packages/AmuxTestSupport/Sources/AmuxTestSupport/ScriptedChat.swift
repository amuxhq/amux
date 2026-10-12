import AmuxCore
import Foundation

/// A chat with no runtime behind it: rows, a frame, a card and what
/// surrounds them set by whoever builds it. The component catalogue draws the real chat views
/// from one, and a test drives a model through it.
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
    private var pending = ChatChanges(keys: [], reloaded: false, session: false)
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
        public var git: GitView?
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
        git: GitView? = nil, changes: Changes? = nil
    ) -> Surroundings {
        let jobs = (0..<(background ?? 0)).map {
            JobRow(command: "job \($0 + 1)", startedAtMs: 0, step: nil)
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

    /// Rows arriving at the newest edge, as the session takes them: while
    /// the reader follows they join the rows and say so on the next take; in
    /// history they are held apart until the reader returns, and the frame
    /// says they are.
    public func append(_ rows: [Row]) {
        lock.withLock {
            if following {
                ordered.append(contentsOf: rows)
                pending.keys.append(contentsOf: rows.map(\.id))
                if ordered.count > cap {
                    ordered.removeFirst(ordered.count - cap)
                    if let oldest = ordered.first { pending.keys.append(oldest.id) }
                }
            } else {
                held.append(contentsOf: rows)
                current.arrivalsHeld = true
                pending.session = true
            }
        }
    }

    /// Puts rows before the oldest, as a page of older history lands, and
    /// says so on the next take.
    public func prepend(_ rows: [Row]) {
        lock.withLock {
            ordered.insert(contentsOf: rows, at: 0)
            pending.keys.append(contentsOf: rows.map(\.id))
        }
    }

    /// Replaces rows in place and says so on the next take.
    public func revise(_ rows: [Row]) {
        lock.withLock {
            for row in rows {
                if let index = ordered.firstIndex(where: { $0.id == row.id }) { ordered[index] = row }
            }
            pending.keys.append(contentsOf: rows.map(\.id))
        }
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

    public func keys() -> [String] { lock.withLock { ordered.map(\.id) } }

    public func keys(above newest: String) -> [String]? {
        lock.withLock {
            guard let index = ordered.firstIndex(where: { $0.id == newest }) else { return nil }
            return ordered[(index + 1)...].map(\.id)
        }
    }

    public func keys(below oldest: String) -> [String]? {
        lock.withLock {
            guard let index = ordered.firstIndex(where: { $0.id == oldest }) else { return nil }
            return ordered[..<index].map(\.id)
        }
    }

    public func oldestKey() -> String? { lock.withLock { ordered.first?.id } }

    /// Where the reader was last said to be, in order, for a test to read.
    public private(set) var follows: [Bool] = []
    private var following = true
    private var held: [Row] = []

    public func follow(_ following: Bool) {
        lock.withLock {
            follows.append(following)
            self.following = following
            guard following, !held.isEmpty else { return }
            ordered.append(contentsOf: held)
            pending.keys.append(contentsOf: held.map(\.id))
            held = []
            current.arrivalsHeld = false
            pending.session = true
        }
    }

    public func rows(for keys: [String], options: RowOptions?) -> [Row] {
        lock.withLock {
            let wanted = Set(keys)
            return ordered.filter { wanted.contains($0.id) }
        }
    }

    public func askCard() -> AskCard? { lock.withLock { card } }
    public func overview() -> Overview? { lock.withLock { facts } }
    public func settings() -> SettingsView? { lock.withLock { offered } }
    public func frame() -> ChatFrame? { lock.withLock { current } }

    public func takeChanges() -> ChatChanges {
        lock.withLock {
            defer { pending = ChatChanges(keys: [], reloaded: false, session: false) }
            return pending
        }
    }

    public func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        lock.withLock { sent.append(draft) }
        return .success(SendOutcome(inputId: [1], state: .sent))
    }

    public func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { .done }
    public func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { .done }
    public func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { .done }
    public func toggleRun(_ member: String, open: [String]) -> [String] {
        open.contains(member) ? open.filter { $0 != member } : open + [member]
    }
    public func keepOpenRuns(_ open: [String]) -> [String] { open }
    public func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? { .done }
    public func withdraw(_ input: [UInt8]) async -> ActOutcome? { .done }
    public func sendNow(_ input: [UInt8]) async -> ActOutcome? { .done }
    public func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    /// Forgets a prompt on its way or refused, as the session does.
    public func discard(_ input: [UInt8]) {
        lock.withLock {
            current.underway.removeAll { $0.inputId == input }
            current.refused.removeAll { $0.inputId == input }
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
