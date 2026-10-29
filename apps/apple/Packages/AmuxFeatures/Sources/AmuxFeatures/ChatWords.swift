import AmuxCore
import Foundation

/// How the phone words the chat's typed facts. The rows, the card and the
/// overview arrive as values; every sentence a person reads about them is
/// written here, so each can be read and tested apart from any view.
public enum ChatWords {
    // MARK: - Numbers

    /// "1m 42s", "8s", "4.2s", "850ms".
    /// Why a typed agent name will not do.
    public static func nameProblem(_ problem: AgentNameProblem) -> String {
        switch problem {
        case .empty: String(localized: "A name cannot be empty")
        case .tooLong: String(localized: "A name is at most 64 characters")
        case .characters: String(localized: "Lowercase letters, digits and hyphens, starting with a letter or digit")
        }
    }

    /// What is wrong with a form field's value, under the field.
    public static func fieldProblem(_ problem: FieldProblem) -> String {
        switch problem {
        case .required: String(localized: "Required")
        case .notANumber: String(localized: "Enter a number")
        case .notWholeNumber: String(localized: "Enter a whole number")
        case .belowMinimum(let minimum): String(localized: "At least \(minimum.formatted())")
        case .aboveMaximum(let maximum): String(localized: "At most \(maximum.formatted())")
        case .tooShort(let length):
            length == 1
                ? String(localized: "At least 1 character") : String(localized: "At least \(length) characters")
        case .tooLong(let length):
            length == 1
                ? String(localized: "At most 1 character") : String(localized: "At most \(length) characters")
        case .tooFew(let count): String(localized: "Pick at least \(count)")
        case .tooMany(let count): String(localized: "Pick at most \(count)")
        }
    }

    public static func duration(_ ms: Int64) -> String {
        let ms = max(0, ms)
        if ms < 1_000 { return "\(ms)ms" }
        let secs = ms / 1_000
        if secs < 60 {
            if ms < 10_000, ms % 1_000 != 0 {
                return String(format: "%.1fs", Double(ms) / 1_000)
            }
            return "\(secs)s"
        }
        let mins = secs / 60
        if mins < 60 { return "\(mins)m \(secs % 60)s" }
        return "\(mins / 60)h \(mins % 60)m"
    }

    /// "148k", "1.2M", "812".
    public static func tokens(_ count: UInt64) -> String {
        switch count {
        case ..<1_000: "\(count)"
        case ..<1_000_000: "\(count / 1_000)k"
        default: String(format: "%.1fM", Double(count) / 1_000_000)
        }
    }

    /// "12 KB", "3.4 MB", "900 B".
    public static func bytes(_ size: UInt64) -> String {
        switch size {
        case ..<1_024: "\(size) B"
        case ..<1_048_576: "\(size / 1_024) KB"
        default: String(format: "%.1f MB", Double(size) / 1_048_576)
        }
    }

    static func plural(_ count: UInt32, _ one: String, _ many: String) -> String {
        count == 1 ? one : many
    }

    // MARK: - Rows

    /// "3 commands · 2 edits · 4 reads", in the terminal's order, with "+"
    /// when the run continues below what this phone holds.
    public static func run(_ run: Run, counts: RunCounts?) -> String {
        let plus = run.openBelow ? "+" : ""
        let counts = counts ?? RunCounts(commands: 0, edits: 0, reads: 0, searches: 0, subagents: 0, other: 0)
        let parts: [String] = [
            (counts.commands, "command", "commands"), (counts.edits, "edit", "edits"),
            (counts.reads, "read", "reads"), (counts.searches, "search", "searches"),
            (counts.subagents, "subagent", "subagents"), (counts.other, "call", "calls"),
        ].compactMap { count, one, many in
            count > 0 ? "\(count)\(plus) " + plural(count, one, many) : nil
        }
        guard parts.isEmpty else { return parts.joined(separator: " · ") }
        return "\(run.steps)\(plus) " + plural(run.steps, "step", "steps")
    }

    /// "5 earlier steps": what a live run keeps folded above its newest.
    public static func earlier(_ steps: UInt32) -> String {
        "\(steps) earlier " + plural(steps, "step", "steps")
    }

    public static func explore(_ verb: ExploreVerb) -> String {
        switch verb {
        case .read: String(localized: "Read")
        case .search: String(localized: "Searched")
        case .list: String(localized: "Listed")
        case .fetch: String(localized: "Fetched")
        case .webSearch: String(localized: "Searched the web")
        }
    }

    /// A call's phase beside it, for a row whose verb does not say it. With
    /// a decision on the row, the decision says what came of asking, so the
    /// phase's word for the same thing is left to it.
    public static func state(_ phase: CallPhase, decided: Bool) -> String? {
        switch phase {
        case .asking, .pending: decided ? nil : String(localized: "waiting")
        case .running: String(localized: "running")
        case .succeeded: nil
        case .failed: String(localized: "failed")
        case .denied: decided ? nil : String(localized: "denied")
        case .cancelled: decided ? nil : String(localized: "cancelled")
        }
    }

    /// A call's verb says what happened to it: asking the person, under way,
    /// refused, cancelled or done.
    public static func verb(
        _ phase: CallPhase, _ row: Row, wants: String, doing: String, done: String
    ) -> String {
        switch phase {
        case .denied: return String(localized: "Denied")
        case .asking: return wants
        case .pending, .running: return doing
        case .cancelled: return String(localized: "Cancelled")
        // A call that went through on a decision says so; its glyph says what it was.
        case .succeeded where row.decision?.outcome == .allowed: return String(localized: "Allowed")
        case .succeeded where row.decision?.outcome == .autoApproved:
            return String(localized: "Auto-approved")
        case .succeeded, .failed: return done
        }
    }

    /// Whether the verb ``verb(_:_:wants:doing:done:)`` gives a call in this
    /// phase already names its decision's outcome.
    public static func verbSays(_ phase: CallPhase, _ outcome: DecisionView) -> Bool {
        switch outcome {
        case .denied: phase == .denied
        case .allowed, .autoApproved: phase == .succeeded
        case .dismissed: false
        }
    }

    /// A permission decision as meta: allowed or denied, its scope, the
    /// note, and where it was answered. The outcome word is left off when
    /// the verb already says it.
    public static func decision(
        _ decision: Decision, verbSaysIt: Bool = false, note: Bool = true
    ) -> String {
        var parts: [String] = []
        if !verbSaysIt {
            let outcome = switch decision.outcome {
            case .allowed: String(localized: "allowed")
            case .denied: String(localized: "denied")
            case .autoApproved: String(localized: "auto-approved")
            case .dismissed: String(localized: "dismissed")
            }
            parts.append(outcome)
        }
        if let granted = decision.granted { parts.append(grant(granted)) }
        if note, let words = decision.note { parts.append("“\(firstLine(words))”") }
        if decision.elsewhere { parts.append(String(localized: "in the terminal")) }
        return parts.joined(separator: " · ")
    }

    /// The row's meta with its decision after it. `verbPhase` is the phase
    /// of a row whose verb comes from ``verb(_:_:wants:doing:done:)``, so the
    /// decision's outcome is left off where that verb says it. A row that
    /// says the person's note on a line of its own leaves it out with
    /// `note: false`.
    public static func meta(
        _ meta: [String], _ row: Row, verbPhase: CallPhase? = nil, note: Bool = true
    ) -> String {
        var kept = meta.filter { !$0.isEmpty }
        if let decision = row.decision {
            let decided = Self.decision(
                decision,
                verbSaysIt: verbPhase.map { verbSays($0, decision.outcome) } ?? false,
                note: note)
            if !decided.isEmpty { kept.append(decided) }
        }
        return kept.joined(separator: " · ")
    }

    public static func answer(_ answer: AnswerView) -> String {
        if answer.hidden { return String(localized: "answered (hidden)") }
        if answer.skipped { return String(localized: "Skipped") }
        var picks = answer.picked
        if let other = answer.other { picks.append("“\(other)”") }
        return picks.joined(separator: ", ")
    }


    public static func resolution(_ resolution: Resolution, answered: String) -> String {
        switch resolution {
        case .open: String(localized: "Asking")
        case .answered: answered
        case .declined: String(localized: "Declined")
        case .cancelled: String(localized: "Cancelled")
        case .replied: String(localized: "Replied instead")
        case .dismissed: String(localized: "Dismissed")
        }
    }

    /// What an allowance granted beyond the one call.
    public static func grant(_ grant: PermissionGrant) -> String {
        switch grant {
        case .claude(let subjects, let directories, _, let modeName, let destination):
            if !subjects.isEmpty {
                String(localized: "always \(subjects.joined(separator: ", ")) \(scope(destination))")
            } else if !directories.isEmpty {
                String(localized: "access to \(directories.joined(separator: ", ")) \(scope(destination))")
            } else if !modeName.isEmpty {
                String(localized: "switched to \(modeName)")
            } else {
                String(localized: "always \(scope(destination))")
            }
        case .session: String(localized: "this session")
        case .always: String(localized: "always, in every project")
        case .commandPrefix(let words):
            String(localized: "commands starting with \(words.joined(separator: " "))")
        case .networkHosts(let hosts):
            hosts.isEmpty
                ? String(localized: "network access")
                : String(localized: "network access to \(hosts.joined(separator: ", "))")
        }
    }

    public static func boundary(_ kind: BoundaryKind, cause: String) -> String {
        let word = switch kind {
        case .started: String(localized: "Started")
        case .cleared: String(localized: "Cleared")
        case .compacted: String(localized: "Compacted")
        case .resumed: String(localized: "Resumed")
        case .forked: String(localized: "Forked")
        case .restarted: String(localized: "Restarted")
        case .exited: String(localized: "Ended")
        case .daemonLost: String(localized: "Lost its daemon")
        case .unspecified: String(localized: "Session")
        }
        return cause.isEmpty ? word : "\(word) · \(cause)"
    }

    /// "Compacted · 148k → 22k".
    public static func compaction(before: UInt64?, after: UInt64?, automatic: Bool) -> String {
        var words = String(localized: "Compacted")
        if let before, let after { words += " · \(tokens(before)) → \(tokens(after))" }
        if automatic { words += " · " + String(localized: "automatic") }
        return words
    }

    /// "Worked 1m 42s".
    public static func turnEnd(durationMs: Int64?, failed: Bool) -> String {
        var words = [durationMs.map { String(localized: "Worked \(duration($0))") }
            ?? String(localized: "Turn ended")]
        if failed { words.append(String(localized: "failed")) }
        return words.joined(separator: " · ")
    }

    public static func thinking(open: Bool, durationMs: Int64?) -> String {
        if open { return String(localized: "Thinking") }
        guard let durationMs else { return String(localized: "Thought") }
        return String(localized: "Thought for \(duration(durationMs))")
    }

    /// "40 tools · 48s".
    public static func subagent(toolCount: UInt32, durationMs: Int64?) -> String {
        var parts = ["\(toolCount) " + plural(toolCount, "tool", "tools")]
        if let durationMs { parts.append(duration(durationMs)) }
        return parts.joined(separator: " · ")
    }

    /// An attachment as its chip names it, from the reference alone.
    public static func chip(_ view: AttachmentView) -> String {
        let (name, detail) = chipParts(view)
        return detail.isEmpty ? name : name + " · " + detail
    }

    /// A chip's name and what it measures, apart: at a large text size the
    /// name wraps and the size goes under it, and neither cuts the other.
    public static func chipParts(_ view: AttachmentView) -> (name: String, detail: String) {
        switch view {
        case .image(let blob), .file(let blob): (blob.name, bytes(blob.size))
        case .text(let name, let lines, _):
            (name, lines == 1 ? String(localized: "1 line") : String(localized: "\(lines) lines"))
        case .review(let comments, _):
            (String(localized: "Review"),
             comments == 1 ? String(localized: "1 comment") : String(localized: "\(comments) comments"))
        case .empty: (String(localized: "Attachment"), "")
        }
    }

    public static func chipGlyph(_ view: AttachmentView) -> String {
        switch view {
        case .image: "photo"
        case .file: "doc"
        case .text: "doc.plaintext"
        case .review: "text.bubble"
        case .empty: "paperclip"
        }
    }

    /// What Codex's automatic reviewer did, as the row's verb; a status this
    /// build does not know in Codex's own word.
    public static func autoReview(_ status: ReviewStatus, _ word: String) -> String {
        switch status {
        case .approved: String(localized: "Auto-approved")
        case .denied: String(localized: "Auto-denied")
        case .inProgress: String(localized: "Auto-reviewing")
        case .timeout: String(localized: "Auto-review timed out")
        case .abort: String(localized: "Auto-review stopped")
        case .unspecified: String(localized: "Auto-reviewed \(word)")
        }
    }

    public static func firstLine(_ text: String) -> String {
        String(text.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first ?? "")
    }

    public static func text(of segments: [Segment]) -> String {
        segments.map { segment in
            switch segment {
            case .text(let text): text
            case .attachment(let view): "[\(chip(view))]"
            }
        }.joined()
    }

    // MARK: - The activity line

    /// "Running cargo test · 12s", as one sentence to hear. `subject` names
    /// the call a Running line points at, when the chat holds it.
    public static func activity(_ kind: ActivityKind, elapsedMs: Int64, subject: String?) -> String {
        let parts = activityParts(kind, elapsedMs: elapsedMs, subject: subject)
        return [[parts.words, parts.subject].compactMap { $0 }.joined(separator: " "), parts.time]
            .compactMap { $0 }.joined(separator: " · ")
    }

    /// The activity line in its three drawn parts: the words, the subject a
    /// Running line names (set in mono), and the elapsed time at the end.
    /// Only the busy states that last a while are timed.
    public static func activityParts(
        _ kind: ActivityKind, elapsedMs: Int64, subject: String?
    ) -> (words: String, subject: String?, time: String?) {
        let elapsed = duration(elapsedMs / 1_000 * 1_000)
        switch kind {
        case .working: return (String(localized: "Working"), nil, elapsed)
        case .thinking: return (String(localized: "Thinking"), nil, nil)
        case .running:
            let named = subject.flatMap { $0.isEmpty ? nil : $0 }
            return (String(localized: "Running"), named, elapsed)
        case .subagents(let count):
            return (count == 1
                ? String(localized: "1 subagent working")
                : String(localized: "\(count) subagents working"), nil, nil)
        case .compacting: return (String(localized: "Compacting"), nil, nil)
        case .safetyCheck: return (String(localized: "Running safety checks"), nil, nil)
        case .retrying(let attempt, let most, _):
            return (most > 0
                ? String(localized: "Retrying · attempt \(attempt) of \(most)")
                : String(localized: "Retrying · attempt \(attempt)"), nil, nil)
        }
    }

    /// What a row names when the activity line points at it.
    public static func subject(of row: Row) -> String? {
        switch row.kind {
        case .command(let command, _, _, _, _, _, _, _): firstLine(command)
        case .explore(_, let subject, _, _): subject
        case .toolCall(let server, let tool, _, _, _): server.isEmpty ? tool : "\(server) · \(tool)"
        case .child(_, let name, _, _, _, _): name.isEmpty ? nil : name
        case .fileChange(let files, _): files.first?.path
        case .subagent(let description, _, _, _, _, _): firstLine(description)
        default: nil
        }
    }

    // MARK: - The composer

    /// What the empty field says: who a message goes to, or why sending
    /// waits while the draft is kept.
    public static func placeholder(
        _ composer: Composer?, agent: String, host: String, reach: Reach, working: Bool
    ) -> String {
        switch composer {
        case .send?, nil:
            return working
                ? String(localized: "Queue a message")
                : String(localized: "Message \(agent)")
        case .resume?:
            return String(localized: "\(agent) has exited · a message resumes it")
        // Not current: a message sent now is held and goes when the chat is.
        case .holds(.catchingUp)?:
            return String(localized: "Catching up · sends once caught up")
        case .holds(.reconnecting)?:
            return String(localized: "Reconnecting · sends when \(host) is back")
        case .holds(.detached)?:
            switch reach {
            case .offline(.revoked):
                return String(localized: "\(host) no longer trusts this phone · your draft is kept")
            case .offline(.signedOut):
                return String(localized: "This phone is signed out · your draft is kept")
            default:
                return String(localized: "\(host) is out of reach · sends when back")
            }
        }
    }

    public static func queued(_ row: QueuedRow) -> String {
        if row.steered { return String(localized: "steered") }
        if let agent = row.fromAgent { return String(localized: "queued from \(agent)") }
        return String(localized: "queued")
    }

    /// How a prompt of this client's is on its way: sending, waiting for
    /// its host while the link is down, or perhaps never arrived.
    public static func underway(_ underway: Underway, host: String) -> String {
        switch underway {
        case .sending(waiting: false): String(localized: "sending")
        case .sending(waiting: true): String(localized: "waiting for \(host)…")
        case .mayNotHaveArrived: String(localized: "may not have arrived")
        case .held: String(localized: "held · sends when \(host) is back")
        }
    }

    /// Where one file starts in a patch of several: its path, and what
    /// happens to it when that is more than an edit.
    public static func fileHeader(_ file: FileRow) -> String {
        switch file.change {
        case .created: String(localized: "\(file.path) (new file)")
        case .deleted: String(localized: "\(file.path) (deleted)")
        case .moved(let to): String(localized: "\(file.path) → \(to)")
        case .edited, .writing: file.path
        }
    }

    // MARK: - Review

    public static func files(_ count: Int) -> String {
        count == 1 ? String(localized: "1 file") : String(localized: "\(count) files")
    }

    public static func comments(_ count: Int) -> String {
        count == 1 ? String(localized: "1 comment") : String(localized: "\(count) comments")
    }

    /// "4 files, 18 added, 28 removed": the counts as a sentence to hear.
    public static func changes(files: Int, added: UInt32, removed: UInt32) -> String {
        String(localized: "\(Self.files(files)), \(added) added, \(removed) removed")
    }

    public static func attachReview(_ count: Int) -> String {
        count == 1
            ? String(localized: "Attach Review · 1 comment")
            : String(localized: "Attach Review · \(count) comments")
    }

    /// "Added line 12, Refused(String),": a diff line as VoiceOver reads it.
    public static func spoken(_ line: DiffLine) -> String {
        let text = line.text.trimmingCharacters(in: .whitespaces)
        switch line.kind {
        case .added: return String(localized: "Added line \(line.newLine ?? 0), \(text)")
        case .removed: return String(localized: "Removed line \(line.oldLine ?? 0), \(text)")
        case .context: return String(localized: "Line \(line.newLine ?? line.oldLine ?? 0), \(text)")
        }
    }

    /// "1 line", "12 lines".
    public static func lines(_ count: UInt32) -> String {
        count == 1 ? String(localized: "1 line") : String(localized: "\(count) lines")
    }

    public static func commentOn(lines: Int) -> String {
        lines == 1 ? String(localized: "Comment on 1 line") : String(localized: "Comment on \(lines) lines")
    }

    // MARK: - The strip

    /// The facts strip's parts, in order; each only while it is true. The
    /// task list is not one of them: it docks as its own card.
    public static func strip(
        context: ContextView?, overview: Overview, now: Date
    ) -> [(text: String, warn: Bool)] {
        var parts: [(String, Bool)] = []
        if let usage = overview.usageNearLimit, usage.blocked {
            parts.append((limitReached(usage, now: now), true))
        }
        if let context, context.nearFull, let percent = context.percent {
            parts.append((String(localized: "\(percent)% context"), true))
        }
        if !overview.jobs.isEmpty {
            parts.append((String(localized: "\(overview.jobs.count) in background"), false))
        }
        if let usage = overview.usageNearLimit, !usage.blocked {
            let near = usage.windows
                .compactMap { window in
                    window.usedPercent.map { "\(usageLabel(window.label)) \(Int($0.rounded()))%" }
                }
                .joined(separator: " · ")
            parts.append((
                near.isEmpty
                    ? String(localized: "Near the usage limit")
                    : String(localized: "Near the usage limit · \(near)"),
                true))
        }
        for server in overview.failedServers {
            parts.append((failed(server), true))
        }
        return parts
    }

    /// "5-hour limit reached · resets 23:24": the window of a usage limit
    /// that has been reached, the fullest one whose use is known when the
    /// provider does not say which. Sending stays open: the provider may still take a prompt.
    public static func limitReached(_ usage: UsageView, now: Date) -> String {
        let window = usage.windows.first { $0.state == .blocked }
            ?? usage.windows.compactMap { window in window.usedPercent.map { (window, $0) } }.max { $0.1 < $1.1 }?.0
        guard let window else { return String(localized: "Usage limit reached") }
        let reached = String(localized: "\(usageLabel(window.label)) reached")
        guard let at = window.resetsAtMs, Date(milliseconds: at) > now else { return reached }
        return String(localized: "\(reached) · resets \(resets(at, now: now))")
    }

    // MARK: - The dock

    /// The task the dock's head names: the one in progress, else the next
    /// to do, else the last one done.
    public static func headTask(_ tasks: TasksView) -> String {
        let pick = tasks.entries.first { $0.mark == .current }
            ?? tasks.entries.first { $0.mark == .todo }
            ?? tasks.entries.last
        return pick?.subject ?? tasks.current
    }

    /// The dock's head when the agent started others but keeps no list.
    public static func started(_ count: Int) -> String {
        count == 1 ? String(localized: "Started 1 agent") : String(localized: "Started \(count) agents")
    }

    /// A started agent's state under its name: how it ended once it has,
    /// else its machine when that is out of reach, else what it is doing.
    public static func childState(_ card: FleetCard) -> String {
        if card.attention != .exited {
            switch card.hostReach {
            case .online: break
            case .away: return String(localized: "\(card.host) away")
            case .offline:
                return String(localized: "\(card.host) offline")
            }
        }
        switch card.attention {
        case .needsYou: return String(localized: "needs you")
        case .working: return String(localized: "working")
        case .starting: return String(localized: "starting")
        case .idle: return String(localized: "idle")
        case .exited:
            switch card.exitCause ?? .ended {
            case .finished: return String(localized: "finished")
            case .ended: return String(localized: "exited")
            case .failed(let cause): return String(localized: "exited · \(cause)")
            }
        }
    }

    /// What stopping an agent cuts, as the stop confirm asks it: "desk-1 is
    /// working and has 2 background jobs and 1 subagent. Stop anyway?" Nil
    /// when a stop cuts nothing under way, for the plain confirm.
    public static func stopCuts(name: String, phase: PhaseView?, jobs: Int, subAgents: Int) -> String? {
        var held: [String] = []
        if jobs > 0 {
            held.append(jobs == 1 ? String(localized: "1 background job") : String(localized: "\(jobs) background jobs"))
        }
        if subAgents > 0 {
            held.append(subAgents == 1 ? String(localized: "1 subagent") : String(localized: "\(subAgents) subagents"))
        }
        let has = held.joined(separator: String(localized: " and "))
        switch phase {
        case .working?:
            return has.isEmpty
                ? String(localized: "\(name) is working. Stop anyway?")
                : String(localized: "\(name) is working and has \(has). Stop anyway?")
        case .needsYou?:
            return has.isEmpty
                ? String(localized: "\(name) needs you mid-turn. Stop anyway?")
                : String(localized: "\(name) needs you mid-turn and has \(has). Stop anyway?")
        default:
            return has.isEmpty ? nil : String(localized: "\(name) has \(has). Stop anyway?")
        }
    }

    /// The exited agent's head in the composer.
    public static func exited(_ cause: ExitCause) -> String {
        switch cause {
        case .finished: String(localized: "Finished")
        case .ended: String(localized: "Exited")
        case .failed(let cause): String(localized: "Exited · \(cause)")
        }
    }

    // MARK: - Settings

    /// A usage window by which limit it is.
    public static func usageLabel(_ label: UsageLabel) -> String {
        switch label {
        case .fiveHour: String(localized: "5-hour limit")
        case .weekly(nil): String(localized: "Weekly limit")
        case .weekly(let model?): String(localized: "\(model) weekly limit")
        case .minutes(let minutes): String(localized: "\(minutes)-minute limit")
        case .named(let name): String(localized: "\(name) limit")
        }
    }

    /// How full a usage window is: "91% used", or "—" when the provider
    /// has not said.
    public static func used(_ window: UsageWindowView) -> String {
        guard let used = window.usedPercent else { return "—" }
        return String(localized: "\(Int(used.rounded()))% used")
    }

    /// A usage window's state and when it resets: "near · resets 16:07".
    /// Nil when there is neither.
    public static func usageDetail(_ window: UsageWindowView, now: Date) -> String? {
        let state: String? = switch window.state {
        case .blocked: String(localized: "reached")
        case .nearLimit: String(localized: "near")
        case .ok: String(localized: "fine")
        case .unknown: nil
        }
        let resets = window.resetsAtMs
            .flatMap { Date(milliseconds: $0) > now ? $0 : nil }
            .map { String(localized: "resets \(Self.resets($0, now: now))") }
        let words = [state, resets].compactMap { $0 }
        return words.isEmpty ? nil : words.joined(separator: " · ")
    }

    /// When a limit resets: the time of day when it is within the day, else
    /// the weekday.
    public static func resets(_ atMs: Int64, now: Date) -> String {
        let at = Date(milliseconds: atMs)
        return at.timeIntervalSince(now) < 20 * 3_600
            ? at.formatted(date: .omitted, time: .shortened)
            : at.formatted(.dateTime.weekday(.abbreviated))
    }

    // MARK: - The overview

    /// What the changes are counted against: "Uncommitted", "vs main".
    public static func comparison(_ comparison: Comparison, base: String?) -> String {
        switch comparison {
        case .uncommitted: String(localized: "Uncommitted")
        case .onBranch: base.map { String(localized: "vs \($0)") } ?? String(localized: "On branch")
        }
    }

    /// A file's counts with a zero side left out: "+20", "−12", "+25 −8".
    public static func counts(added: UInt32, removed: UInt32) -> String {
        [added > 0 ? "+\(added)" : nil, removed > 0 ? "\u{2212}\(removed)" : nil]
            .compactMap { $0 }.joined(separator: " ")
    }

    /// The question before one background job is stopped, naming it:
    /// "Stop cargo test?".
    public static func stopJob(_ job: JobRow) -> String {
        String(localized: "Stop \(firstLine(job.command))?")
    }

    /// What stopping one background job leaves alone.
    public static let stopJobLeaves = String(localized: "The agent keeps running.")

    /// A background job asked to stop that has not yet ended.
    public static let stoppingJob = String(localized: "Stopping…")

    /// How long a background job has run: "40s", "14m".
    public static func ran(since startedAtMs: Int64, now: Date) -> String {
        Elapsed.spelled(max(0, now.timeIntervalSince(Date(milliseconds: startedAtMs))))
    }

    /// A tool server that failed: "github needs sign-in".
    public static func failed(_ server: ServerView) -> String {
        server.needsAuth
            ? String(localized: "\(server.name) needs sign-in")
            : String(localized: "\(server.name) failed to start")
    }

    /// The model chip's words, from the controls the view sums up: the
    /// model by the one name the view gives it, its effort, and the
    /// permission and mode only while they are not the agent's normal ones.
    /// Nil when nothing is reported.
    public static func chip(_ controls: ControlsSummary)
        -> (model: String, detail: String, unusual: [String])?
    {
        let unusual = [controls.permission.map(permission), controls.mode.map(mode)]
            .compactMap { $0 }
        let model = controls.model ?? ""
        let detail = controls.effort ?? ""
        if model.isEmpty && detail.isEmpty && unusual.isEmpty { return nil }
        return (model, detail, unusual)
    }

    /// A permission by the name the view gives it; Codex settings no name
    /// fits are custom.
    public static func permission(_ choice: PermissionChoice) -> String {
        choice.custom ? String(localized: "Custom") : choice.displayName
    }

    /// What the catalogue says of a permission beyond its name: that it
    /// acts without asking, that the agent reported it without offering
    /// it, or that it cannot be picked from here.
    public static func permissionDetail(_ choice: PermissionChoice) -> String {
        if choice.unlisted { return String(localized: "Reported by the agent") }
        if choice.neverAsks { return String(localized: "Acts without asking") }
        if !choice.settable { return String(localized: "Can’t be picked from here") }
        return ""
    }

    /// A mode by the name the view gives it.
    public static func mode(_ choice: ModeChoice) -> String { choice.displayName }

    /// How the model and effort change where only the agent's own commands
    /// change them, typed in the composer; nil where a pick does.
    public static func byTyping(_ changeable: Changeability) -> String? {
        switch (changeable.model, changeable.effort) {
        case (.byTyping(let model), .byTyping(let effort)):
            String(localized: "To change the model or effort, type \(model) <name> or \(effort) <level> in the composer.")
        case (.byTyping(let model), _):
            String(localized: "To change the model, type \(model) <name> in the composer.")
        case (_, .byTyping(let effort)):
            String(localized: "To change the effort, type \(effort) <level> in the composer.")
        default: nil
        }
    }

    /// Where the permission changes only by stepping through the agent's
    /// own cycle.
    public static var cyclesOnly: String {
        String(localized: "Terminal Claude changes permission only by cycling through its permissions.")
    }

    /// The permissions card's heading, by the agent's kind.
    public static func permissionsHeading(_ kind: Kind?) -> String {
        kind == .codex ? String(localized: "CODEX PERMISSIONS") : String(localized: "CLAUDE PERMISSIONS")
    }

    /// A command as VoiceOver speaks it: its name, then where it comes from.
    public static func command(_ command: CommandView) -> String {
        command.source.isEmpty
            ? command.name
            : String(localized: "\(command.name), from \(command.source)")
    }

    /// The plus menu's permissions row: the current permission after the
    /// name.
    public static func permissionsItem(_ settings: SettingsView) -> String {
        guard let current = settings.permissions.first(where: { $0.current }) else {
            return String(localized: "Permissions")
        }
        return String(localized: "Permissions · \(permission(current))")
    }

    /// Who must be signed in again, for the card that stands in the
    /// composer's place.
    public static func needsSignIn(_ kind: Kind?) -> String {
        kind == .codex
            ? String(localized: "Codex needs you to sign in")
            : String(localized: "Claude needs you to sign in")
    }

    /// Where and how to sign in again: on the agent's own host, since that
    /// is where the provider keeps its credential.
    public static func signInSteps(_ kind: Kind?, host: String) -> String {
        kind == .codex
            ? String(localized: "Run codex login on \(host).")
            : String(localized: "Run claude and sign in with /login on \(host).")
    }

    public static func signIn(_ view: SignInView) -> String {
        let head = switch view.state {
        case .expired: String(localized: "Sign-in expired")
        case .failed: String(localized: "Sign-in failed")
        default: String(localized: "Signed out")
        }
        return view.account.isEmpty ? head : "\(head) · \(view.account)"
    }

    // MARK: - The card

    /// What the card says it wants.
    public static func headline(_ card: AskCard) -> String {
        switch card.body {
        case .command: return String(localized: "Wants to run a command")
        case .edit(_, let files, _, _, _, _, _, _) where files > 1:
            return String(localized: "Wants to edit \(files) files")
        case .edit(_, _, _, _, _, _, true?, _): return String(localized: "Wants to create a file")
        case .edit: return String(localized: "Wants to edit a file")
        case .tool(let server, let tool, _):
            return server.isEmpty
                ? String(localized: "Wants to use \(tool)")
                : String(localized: "Wants to use \(server) · \(tool)")
        case .question(let questions) where questions.count > 1:
            return String(localized: "\(questions.count) questions")
        case .question: return String(localized: "Question")
        case .plan: return String(localized: "Plan ready")
        case .form(let server, _, _): return String(localized: "\(server) needs details")
        case .link(let server, _, _, _): return String(localized: "\(server) wants you to open a link")
        case .access: return String(localized: "Wants more access")
        case .unanswerable: return String(localized: "Can’t answer this here")
        }
    }

    static func scope(_ scope: Scope) -> String {
        switch scope {
        case .session: String(localized: "this session")
        case .project: String(localized: "in this project")
        case .projectShared: String(localized: "in this project, for everyone")
        case .user: String(localized: "in every project")
        case .other(let other): other
        }
    }

    /// A choice in words: what happens, never rule syntax.
    public static func choice(_ choice: Choice) -> String {
        let label: String = switch choice.outcome {
        case .allowOnce: String(localized: "Allow once")
        case .allowAlways(let subjects, let directories, let mode, let modeName, let scope):
            if !subjects.isEmpty {
                String(localized: "Always allow \(subjects.joined(separator: ", ")) \(Self.scope(scope))")
            } else if !directories.isEmpty {
                String(localized: "Allow access to \(directories.joined(separator: ", ")) \(Self.scope(scope))")
            } else if !mode.isEmpty {
                String(localized: "Switch to \(modeName.isEmpty ? mode : modeName)")
            } else {
                String(localized: "Always allow \(Self.scope(scope))")
            }
        case .allowForSession: String(localized: "Allow for this session")
        case .allowSimilar(let prefix):
            String(localized: "Allow commands starting with \(prefix.joined(separator: " "))")
        case .allowNetwork(let hosts):
            hosts.isEmpty
                ? String(localized: "Allow network access")
                : String(localized: "Allow network access to \(hosts.joined(separator: ", "))")
        case .deny(let stops): stops ? String(localized: "Deny and stop") : String(localized: "Deny")
        case .denyAndStop: String(localized: "Deny and stop")
        case .approvePlan(let mode, let modeName):
            mode.isEmpty
                ? String(localized: "Approve")
                : String(localized: "Approve and switch to \(modeName.isEmpty ? mode : modeName)")
        case .sendBack: String(localized: "Send back")
        case .submit: String(localized: "Submit")
        case .decline: String(localized: "Decline")
        case .openLink: String(localized: "I’m done")
        case .grantForTurn: String(localized: "Grant")
        case .grantForSession: String(localized: "Grant")
        }
        return choice.takesNote ? label + "…" : label
    }

    /// A plan approval's switch as the plan card's toggle names it, "Switch
    /// to Auto": the permission by the agent's own name for it.
    public static func planSwitch(_ choice: Choice) -> String {
        guard case .approvePlan(let mode, let modeName) = choice.outcome else { return "" }
        return String(localized: "Switch to \(modeName.isEmpty ? mode : modeName)")
    }

    /// A choice on a button: the likely allow reads "Allow" beside Deny,
    /// and terminal Claude's one deny reads "Deny" though it always stops.
    public static func button(_ choice: Choice) -> String {
        switch choice.outcome {
        case .allowOnce: String(localized: "Allow")
        case .deny: String(localized: "Deny")
        default: Self.choice(choice)
        }
    }

    /// An offered scope as a row: what it allows, and under it how far it
    /// reaches.
    public static func scopeRow(_ choice: Choice) -> (title: String, detail: String?) {
        switch choice.outcome {
        case .allowAlways(let subjects, let directories, let mode, let modeName, let scope):
            if !subjects.isEmpty {
                return (String(localized: "Always allow \(subjects.joined(separator: ", "))"), reach(scope))
            } else if !directories.isEmpty {
                // The folder is the long part: it goes under the outcome.
                return (scope == .session
                            ? String(localized: "Allow access for this session")
                            : String(localized: "Always allow access"),
                        String(localized: "in \(directories.joined(separator: ", "))"))
            } else if !mode.isEmpty {
                return (String(localized: "Switch to \(modeName.isEmpty ? mode : modeName)"), reach(scope))
            }
            return (String(localized: "Always allow"), reach(scope))
        case .allowSimilar(let prefix):
            return (String(localized: "Allow similar commands"),
                    String(localized: "Starting with \(prefix.joined(separator: " "))"))
        case .allowNetwork(let hosts):
            return (String(localized: "Allow network access"),
                    hosts.isEmpty ? nil : String(localized: "To \(hosts.joined(separator: ", "))"))
        default:
            return (Self.choice(choice), nil)
        }
    }

    /// How far a scope reaches, under its row.
    static func reach(_ scope: Scope) -> String {
        switch scope {
        case .session: String(localized: "For this session")
        case .project: String(localized: "In this project, from now on")
        case .projectShared: String(localized: "In this project, for everyone")
        case .user: String(localized: "In every project, from now on")
        case .other(let other): other
        }
    }

    /// "1 of 3" when asks are queued.
    public static func position(_ card: AskCard) -> String? {
        card.count > 1 ? String(localized: "\(card.position) of \(card.count)") : nil
    }
}
