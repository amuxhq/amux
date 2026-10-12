import AmuxValues
import Foundation

/// Half-typed messages kept on this phone, one per account and agent, so a
/// draft outlives its chat's session closing and the app being reopened.
/// They stay on the device: a draft is never synced to another one.
///
/// Each draft is one small file under the app's support directory, written
/// when the draft changes and removed when it empties, so a launch reads
/// back exactly what the person left.
public struct DraftShelf: Sendable {
    private let folder: URL

    public init(account: AccountId, root: URL = DraftShelf.root) {
        folder = root.appendingPathComponent(Self.hex(Array(account.value.utf8)), isDirectory: true)
    }

    /// Where every account's drafts live.
    public static let root: URL = FileManager.default
        .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("amux/drafts", isDirectory: true)

    /// The draft left for an agent, if any.
    public func draft(for agent: AgentKey) -> Draft? {
        guard let data = try? Data(contentsOf: file(agent)) else { return nil }
        return try? JSONDecoder().decode(Draft.self, from: data)
    }

    /// Keeps an agent's draft as it now stands; an empty one is forgotten.
    public func keep(_ draft: Draft, for agent: AgentKey) {
        let url = file(agent)
        guard !draft.parts.isEmpty else {
            try? FileManager.default.removeItem(at: url)
            return
        }
        guard let data = try? JSONEncoder().encode(draft) else { return }
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try? data.write(to: url, options: .atomic)
    }

    /// Forgets every draft of this account, as it leaves the phone.
    public func forgetAll() {
        try? FileManager.default.removeItem(at: folder)
    }

    private func file(_ agent: AgentKey) -> URL {
        folder.appendingPathComponent("\(Self.hex(agent.host))-\(Self.hex(agent.agent)).json")
    }

    private static func hex(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}
