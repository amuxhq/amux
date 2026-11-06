import Foundation

/// What a push says an agent wants of the person, as the daemon's serde writes
/// it: each case under its snake-case name, and `signed_out`, which carries
/// nothing, as a bare string.
public enum PushNotice: Decodable, Equatable, Sendable {
    case ask(wants: PushWants, why: String, more: UInt32)
    case signedOut
    case usageLimit(resetsAtMs: Int64?)
    case failed(message: String)
    case died(detail: String)

    private enum Keys: String, CodingKey {
        case ask
        case usageLimit = "usage_limit"
        case failed
        case died
    }

    private enum AskKeys: String, CodingKey { case wants, why, more }
    private enum UsageKeys: String, CodingKey { case resetsAtMs = "resets_at_ms" }
    private enum FailedKeys: String, CodingKey { case message }
    private enum DiedKeys: String, CodingKey { case detail }

    public init(from decoder: any Decoder) throws {
        if let word = try? decoder.singleValueContainer().decode(String.self) {
            guard word == "signed_out" else {
                throw DecodingError.dataCorrupted(.init(
                    codingPath: decoder.codingPath, debugDescription: "not a notice: \(word)"))
            }
            self = .signedOut
            return
        }
        let container = try decoder.container(keyedBy: Keys.self)
        if container.contains(.ask) {
            let ask = try container.nestedContainer(keyedBy: AskKeys.self, forKey: .ask)
            self = .ask(
                wants: try ask.decode(PushWants.self, forKey: .wants),
                why: try ask.decode(String.self, forKey: .why),
                more: try ask.decode(UInt32.self, forKey: .more))
        } else if container.contains(.usageLimit) {
            let usage = try container.nestedContainer(keyedBy: UsageKeys.self, forKey: .usageLimit)
            self = .usageLimit(resetsAtMs: try usage.decodeIfPresent(Int64.self, forKey: .resetsAtMs))
        } else if container.contains(.failed) {
            let failed = try container.nestedContainer(keyedBy: FailedKeys.self, forKey: .failed)
            self = .failed(message: try failed.decode(String.self, forKey: .message))
        } else {
            let died = try container.nestedContainer(keyedBy: DiedKeys.self, forKey: .died)
            self = .died(detail: try died.decode(String.self, forKey: .detail))
        }
    }
}

/// What an ask wants, each case under its snake-case name.
public enum PushWants: Decodable, Equatable, Sendable {
    case run(command: String)
    case change(path: String, files: UInt32)
    case use(server: String, tool: String)
    case question(text: String)
    case plan(title: String)
    /// A form, a link or access: the provider's own sentence.
    case other(text: String)

    private enum Keys: String, CodingKey { case run, change, use, question, plan, other }
    private enum RunKeys: String, CodingKey { case command }
    private enum ChangeKeys: String, CodingKey { case path, files }
    private enum UseKeys: String, CodingKey { case server, tool }
    private enum TextKeys: String, CodingKey { case text }
    private enum PlanKeys: String, CodingKey { case title }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Keys.self)
        if container.contains(.run) {
            let run = try container.nestedContainer(keyedBy: RunKeys.self, forKey: .run)
            self = .run(command: try run.decode(String.self, forKey: .command))
        } else if container.contains(.change) {
            let change = try container.nestedContainer(keyedBy: ChangeKeys.self, forKey: .change)
            self = .change(
                path: try change.decode(String.self, forKey: .path),
                files: try change.decode(UInt32.self, forKey: .files))
        } else if container.contains(.use) {
            let use = try container.nestedContainer(keyedBy: UseKeys.self, forKey: .use)
            self = .use(
                server: try use.decode(String.self, forKey: .server),
                tool: try use.decode(String.self, forKey: .tool))
        } else if container.contains(.question) {
            let question = try container.nestedContainer(keyedBy: TextKeys.self, forKey: .question)
            self = .question(text: try question.decode(String.self, forKey: .text))
        } else if container.contains(.plan) {
            let plan = try container.nestedContainer(keyedBy: PlanKeys.self, forKey: .plan)
            self = .plan(title: try plan.decode(String.self, forKey: .title))
        } else {
            let other = try container.nestedContainer(keyedBy: TextKeys.self, forKey: .other)
            self = .other(text: try other.decode(String.self, forKey: .text))
        }
    }
}
