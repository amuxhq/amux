import UserNotifications

/// Writes what an opened push says over the plain alert the service sent.
public enum PushAlert {
    /// The name and words replace the placeholder; `amux` is left naming the
    /// host and agent, which is what a tap and the app's clearing of answered
    /// notifications read; and an agent's notifications group together.
    public static func write(_ push: OpenedPush, into content: UNMutableNotificationContent) {
        if !push.name.isEmpty { content.title = push.name }
        content.body = PushWords.body(push.notice)
        content.threadIdentifier = "\(push.host)/\(push.agent)"
        content.userInfo["amux"] = ["host": push.host, "agent": push.agent]
    }
}
