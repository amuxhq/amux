import AmuxValues

/// Where an agent a call opened stands, as the call's row says it.
///
/// A call can return while the agent it started runs on, and only the
/// fleet knows of an ask that agent waits on, so the agent's fleet row
/// decides this whenever this client's fleet holds it. The call's own
/// state stands in only when the fleet does not, or for a call from
/// history that names no agent.
public enum AgentStanding: Equatable, Sendable {
    case running
    case needsYou
    /// Live and waiting for its parent, as an amux child between turns.
    case idle
    case done
    case stopped
    case failed

    public init(_ phase: PhaseView) {
        switch phase {
        case .starting, .working: self = .running
        case .needsYou: self = .needsYou
        case .idle: self = .idle
        case .exited(.finished): self = .done
        case .exited(.ended): self = .stopped
        case .exited(.failed): self = .failed
        }
    }

    public init(_ call: CallPhase) {
        switch call {
        case .pending, .running: self = .running
        case .asking: self = .needsYou
        case .succeeded: self = .done
        case .failed: self = .failed
        case .cancelled, .denied: self = .stopped
        }
    }

    /// The agent `opened` when the fleet holds it, else the call's `state`.
    public static func of(_ opened: OpenedAgent?, call state: CallPhase) -> AgentStanding {
        opened.map { AgentStanding($0.phase) } ?? AgentStanding(state)
    }

    /// "running", "waiting on you", "done", as the row's meta.
    public var words: String {
        switch self {
        case .running: String(localized: "running")
        case .needsYou: String(localized: "waiting on you")
        case .idle: String(localized: "idle")
        case .done: String(localized: "done")
        case .stopped: String(localized: "stopped")
        case .failed: String(localized: "failed")
        }
    }

    /// Still at work, so a row draws it bright.
    public var live: Bool { self == .running }
}

extension ChatWords {
    /// "4 subagents": the one row the calls of a turn fold into.
    public static func subagentGroup(_ count: UInt32) -> String {
        count == 1 ? String(localized: "1 subagent") : String(localized: "\(count) subagents")
    }

    /// "2 running · 1 waiting on you · 1 done": what a group's agents come
    /// to, by where each stands, in the order a reader acts on them.
    public static func subagentGroupState(_ state: GroupState) -> String {
        subagentGroupParts(state).map { "\($0.count) \($0.standing.words)" }.joined(separator: " · ")
    }

    /// The counts `subagentGroupState` says, apart, so a client can ink each.
    /// Agents this client's fleet does not hold yet are left unsaid rather
    /// than guessed at.
    public static func subagentGroupParts(_ state: GroupState) -> [(count: UInt32, standing: AgentStanding)] {
        [
            (state.running, AgentStanding.running), (state.needsYou, .needsYou), (state.idle, .idle),
            (state.finished, .done), (state.ended, .stopped), (state.failed, .failed),
        ]
        .filter { $0.0 > 0 }
        .map { (count: $0.0, standing: $0.1) }
    }

    /// The one standing a group's mark takes: the person first, then work
    /// under way, then a failure, else done.
    public static func subagentGroupStanding(_ state: GroupState) -> AgentStanding {
        if state.needsYou > 0 { return .needsYou }
        if state.running > 0 { return .running }
        if state.failed > 0 { return .failed }
        if state.finished == 0 && state.ended > 0 { return .stopped }
        return .done
    }

    /// "Claude runs this subagent": the head of a provider subagent's
    /// composer, which has no text box because the provider lets only the
    /// parent's model talk to it.
    public static func nativeRunner(_ kind: Kind?) -> String {
        switch kind {
        case .claudeSdk?, .claudePty?: String(localized: "Claude runs this subagent")
        case .codex?: String(localized: "Codex runs this subagent")
        case .unspecified?, nil: String(localized: "Its provider runs this subagent")
        }
    }

    /// Under the head: why there is nowhere to type.
    public static let nativeTakesNoMessages = String(localized: "It takes no messages.")
}

extension Composer {
    /// A provider subagent's own Stop, when the chat is one; nil for every
    /// other chat, which has a text box.
    public var nativeStop: JobStop? {
        if case .native(let stop) = self { return stop }
        return nil
    }
}
