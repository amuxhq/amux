import AmuxPush
import AmuxValues
import Foundation

/// What a "needs you" notification names: the agent, and the host it runs on.
///
/// A push arrives sealed, addressed to the profile it is for:
///
///     {"aps": {"content-available": 1, "alert": {…}},
///      "amux": {"to": "<host id>", "sealed": "<base64>"}}
///
/// The notification extension opens it and leaves the two ids in its place,
/// under the same key, which is what a tap and the clearing of answered
/// notifications read:
///
///     {"amux": {"host": "<uuid>", "agent": "<uuid>"}}
///
/// A background wake gets the raw payload, the extension's work being on a
/// copy, so the app opens that itself.
public enum PushPayload {
    /// The agent a payload names. `keys` opens a payload still sealed, and
    /// without them, or with none that fits, a sealed payload names nobody.
    public static func agent(
        _ payload: [AnyHashable: Any], keys: (any PushKeys)? = nil
    ) -> AgentKey? {
        if let amux = payload["amux"] as? [String: Any],
           let named = key(host: amux["host"] as? String, agent: amux["agent"] as? String) {
            return named
        }
        guard let keys, let opened = SealedPush.open(payload, keys: keys) else { return nil }
        return key(host: opened.host, agent: opened.agent)
    }

    private static func key(host: String?, agent: String?) -> AgentKey? {
        guard let host = host.flatMap(HostId.init),
              let agent = agent.flatMap(UUID.init(uuidString:)) else { return nil }
        return AgentKey(host: host, agent: agent)
    }

    /// Which of the notifications on show, each its identifier and payload,
    /// are for one of `settled`: agents that no longer need you, whose
    /// notifications would open onto a chat that has moved on.
    public static func stale(
        _ delivered: [(id: String, payload: [AnyHashable: Any])], settled: Set<AgentKey>
    ) -> [String] {
        delivered.compactMap { notification in
            agent(notification.payload).flatMap { settled.contains($0) ? notification.id : nil }
        }
    }
}
