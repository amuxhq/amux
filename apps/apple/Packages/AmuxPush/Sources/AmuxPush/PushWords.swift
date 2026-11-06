import Foundation

/// What a notification says: the agent's name over what it wants of the
/// person, in the words its row on the fleet uses (`FleetWords` in the app,
/// which the extension can't link).
public enum PushWords {
    public static func body(_ notice: PushNotice, now: Date = Date()) -> String {
        switch notice {
        case .ask(let wants, _, let more):
            let words = self.wants(wants)
            return more > 0 ? String(localized: "\(words) · and \(more) more") : words
        // Says no provider because a notice names none.
        case .signedOut:
            return String(localized: "Signed out of its provider · sign in on its host")
        case .usageLimit(let resetsAtMs):
            guard let resetsAtMs else { return String(localized: "Usage limit reached") }
            let at = Date(timeIntervalSince1970: TimeInterval(resetsAtMs) / 1000)
            guard at > now else { return String(localized: "Usage limit reached") }
            return String(localized: "Usage limit reached · resets \(resets(at, now: now))")
        case .failed(let message):
            let said = firstLine(message)
            return said.isEmpty ? String(localized: "Its turn failed") : said
        case .died(let detail):
            let said = firstLine(detail)
            return said.isEmpty
                ? String(localized: "Stopped unexpectedly")
                : String(localized: "Stopped unexpectedly: \(said)")
        }
    }

    private static func wants(_ wants: PushWants) -> String {
        let words: String = switch wants {
        case .run(let command):
            String(localized: "Wants to run \(firstLine(command))")
        case .change(_, let files) where files > 1:
            String(localized: "Wants to edit \(files) files")
        case .change(let path, _):
            String(localized: "Wants to edit \(path)")
        case .use(let server, let tool):
            String(localized: "Wants to use \([server, tool].filter { !$0.isEmpty }.joined(separator: " "))")
        case .question(let text):
            firstLine(text)
        case .plan(let title) where title.isEmpty:
            String(localized: "Has a plan for you to decide")
        case .plan(let title):
            String(localized: "Has a plan for you to decide: \(firstLine(title))")
        case .other(let text):
            firstLine(text)
        }
        return words.isEmpty ? String(localized: "Needs you") : words
    }

    /// When a limit resets: the time of day when it is within the day, else
    /// the weekday, as the fleet says it.
    private static func resets(_ at: Date, now: Date) -> String {
        at.timeIntervalSince(now) < 20 * 3_600
            ? at.formatted(date: .omitted, time: .shortened)
            : at.formatted(.dateTime.weekday(.abbreviated))
    }

    private static func firstLine(_ text: String) -> String {
        String(text.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first ?? "")
    }
}
