import AmuxCore
import AmuxPush
import AmuxValues
import Foundation

/// What a fleet row says under its name: the words for the runtime's
/// second line, as every client says them.
public enum FleetWords {
    /// How loud a second line reads. What an agent asks is the loudest line
    /// on home; a line that says the agent cannot go on warns.
    public enum Ink: Equatable, Sendable {
        case ask
        case quiet
        case warning
        case error
    }

    /// Nil when there is nothing to say (starting, or nothing known yet),
    /// and for what the row's state word already says: that its host is
    /// away, or that it exited with no cause.
    ///
    /// Jobs the agent runs in the background leave it idle, so an idle or
    /// working line leads with how many there are, ahead of anything a
    /// long line could cut off.
    public static func secondLine(
        _ line: SecondLine, kind: Kind, now: Date, backgroundJobs: UInt32 = 0
    ) -> (text: String, ink: Ink)? {
        if backgroundJobs > 0 {
            let count = backgroundJobs == 1
                ? String(localized: "1 background job")
                : String(localized: "\(backgroundJobs) background jobs")
            switch line {
            case .lastSaid(let said) where !said.isEmpty: return ("\(count) · \(said)", .quiet)
            case .step(let step): return ("\(count) · \(self.step(step))", .quiet)
            case .lastSaid, .blank: return (String(localized: "\(count) running"), .quiet)
            default: break
            }
        }
        switch line {
        case .said(let said):
            let ink: Ink = if case .ask = said { .ask } else { .warning }
            return (PushWords.body(said, provider: provider(kind), now: now), ink)
        case .step(let step): return (self.step(step), .quiet)
        case .lastSaid(let said): return said.isEmpty ? nil : (said, .quiet)
        case .exited(.failed(let cause)): return (String(localized: "Stopped · \(cause)"), .error)
        case .hostRevoked:
            return (String(localized: "Its host no longer trusts this phone · pair again to reach it"), .warning)
        case .exited(.finished), .exited(.ended), .blank: return nil
        }
    }

    /// The step a working agent is on, as the chat's activity line names
    /// it: the step's own subject when it is running one.
    public static func step(_ line: AmuxValues.ActivityLine) -> String {
        if case .running = line.activity.kind, let step = line.step, !step.isEmpty {
            return ChatWords.firstLine(step)
        }
        return ChatWords.activityParts(line.activity.kind, elapsedMs: 0, subject: nil).words
    }

    /// The provider a sign-in belongs to.
    static func provider(_ kind: Kind) -> String {
        switch kind {
        case .claudeSdk, .claudePty: "Claude"
        case .codex: "Codex"
        case .unspecified: String(localized: "the provider")
        }
    }
}
