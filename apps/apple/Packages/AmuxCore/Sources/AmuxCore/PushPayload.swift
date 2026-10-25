import AmuxValues
import Foundation

/// What a "needs you" notification names: the agent, and the host it runs
/// on, each as its UUID under the payload's `amux` key:
///
///     {"aps": {"content-available": 1, "alert": {…}},
///      "amux": {"host": "<uuid>", "agent": "<uuid>"}}
///
/// The account service builds the payload from what the agent's daemon
/// hands it; the phone reads nothing else from it.
public enum PushPayload {
    public static func agent(_ payload: [AnyHashable: Any]) -> AgentKey? {
        guard let amux = payload["amux"] as? [String: Any],
              let host = (amux["host"] as? String).flatMap(HostId.init),
              let agent = (amux["agent"] as? String).flatMap(UUID.init(uuidString:))
        else { return nil }
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
