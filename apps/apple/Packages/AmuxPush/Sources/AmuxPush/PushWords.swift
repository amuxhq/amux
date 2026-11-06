import Foundation

/// What an agent wants of the person, in words. A notification says it and
/// so does the agent's row on the fleet, from this one place, because the
/// extension that writes a notification can link nothing of the app's.
public enum PushWords {
    /// `provider` is the word for the agent's provider, which a row knows and
    /// a push doesn't: a notice names none.
    public static func body(_ said: Said, provider: String? = nil, now: Date = Date()) -> String {
        switch said {
        case .ask(_, let wants, _, let more):
            let words = self.wants(wants)
            return more > 0 ? String(localized: "\(words) · \(more) more") : words
        case .signedOut:
            return provider.map { String(localized: "Signed out of \($0)") }
                ?? String(localized: "Signed out of its provider · sign in on its host")
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

    /// The provider's reason for an ask, which a notification has room for
    /// and a row doesn't.
    public static func reason(_ said: Said) -> String? {
        guard case .ask(_, _, let why, _) = said else { return nil }
        let reason = firstLine(why)
        return reason.isEmpty ? nil : reason
    }

    private static func wants(_ wants: Wants) -> String {
        let words: String = switch wants {
        case .run(let command):
            String(localized: "Wants to run \(firstLine(command))")
        case .change(_, let files, _) where files > 1:
            String(localized: "Wants to edit \(files) files")
        case .change(let path, _, let created?):
            created ? String(localized: "Wants to create \(path)") : String(localized: "Wants to edit \(path)")
        case .change(let path, _, nil):
            String(localized: "Wants to write \(path)")
        case .use(let server, let tool):
            String(localized: "Wants to use \([server, tool].filter { !$0.isEmpty }.joined(separator: " "))")
        case .question(let text):
            firstLine(text)
        case .plan(let title) where title.isEmpty:
            String(localized: "Has a plan for you to decide")
        case .plan(let title):
            String(localized: "Has a plan for you to decide: \(firstLine(title))")
        case .access(let reason):
            String(localized: "Wants access: \(firstLine(reason))")
        case .other(let server, let text):
            server.isEmpty ? firstLine(text) : "\(server): \(firstLine(text))"
        }
        return words.isEmpty ? String(localized: "Needs you") : words
    }

    /// When a limit resets: the time of day when it is within the day, else
    /// the weekday.
    public static func resets(_ at: Date, now: Date) -> String {
        at.timeIntervalSince(now) < 20 * 3_600
            ? at.formatted(date: .omitted, time: .shortened)
            : at.formatted(.dateTime.weekday(.abbreviated))
    }

    private static func firstLine(_ text: String) -> String {
        String(text.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first ?? "")
    }
}
