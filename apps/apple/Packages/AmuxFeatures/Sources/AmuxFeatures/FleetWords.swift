import AmuxCore
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
        case .ask(let ask): return (self.ask(ask), .ask)
        case .step(let step): return (self.step(step), .quiet)
        case .lastSaid(let said): return said.isEmpty ? nil : (said, .quiet)
        case .stuck(.signedOut(let state, _)):
            let provider = provider(kind)
            let words = switch state {
            case .expired: String(localized: "\(provider) sign-in expired")
            case .failed: String(localized: "\(provider) sign-in failed")
            default: String(localized: "Signed out of \(provider)")
            }
            return (words, .warning)
        case .stuck(.usageLimit(let resetsAtMs)):
            guard let at = resetsAtMs, Date(milliseconds: at) > now else {
                return (String(localized: "Usage limit reached"), .warning)
            }
            return (
                String(localized: "Usage limit reached · resets \(ChatWords.resets(at, now: now))"),
                .warning)
        case .exited(.failed(let cause)): return (cause, .error)
        case .hostRevoked:
            return (String(localized: "Its host no longer trusts this phone · pair again to reach it"), .warning)
        case .exited(.finished), .exited(.ended), .blank: return nil
        }
    }

    /// What an ask asks, in a line: the command, the file, the question.
    public static func ask(_ ask: AskSummary) -> String {
        let words: String = switch ask.subject {
        case .command(let command): String(localized: "Wants to run \(ChatWords.firstLine(command))")
        case .edit(_, let files, _) where files > 1: String(localized: "Wants to edit \(files) files")
        case .edit(let path, _, let created?):
            created ? String(localized: "Wants to create \(path)") : String(localized: "Wants to edit \(path)")
        case .edit(let path, _, nil): String(localized: "Wants to write \(path)")
        case .tool(let server, let tool):
            String(localized: "Wants to use \([server, tool].filter { !$0.isEmpty }.joined(separator: " "))")
        case .question(let question, _): ChatWords.firstLine(question)
        case .plan: String(localized: "Has a plan for you to decide")
        case .form(let server, let message), .link(let server, let message):
            server.isEmpty ? ChatWords.firstLine(message) : "\(server): \(ChatWords.firstLine(message))"
        case .access(let reason): String(localized: "Wants access: \(ChatWords.firstLine(reason))")
        case .unanswerable(let reason): ChatWords.firstLine(reason)
        }
        return ask.count > 1 ? String(localized: "\(words) · \(ask.count - 1) more") : words
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
