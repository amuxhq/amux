import AmuxPush
import UserNotifications

/// Opens a push on its way to the screen and writes what it says over the
/// plain alert.
///
/// All of it is synchronous and takes milliseconds, so there is nothing for
/// the system's expiry to interrupt. A push that can't be opened, as before
/// the first unlock after a restart, goes on as the plain alert it arrived as.
final class NotificationService: UNNotificationServiceExtension {
    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler handler: @escaping (UNNotificationContent) -> Void
    ) {
        guard let opened = SealedPush.open(request.content.userInfo, keys: PushKeychain()),
              let content = request.content.mutableCopy() as? UNMutableNotificationContent
        else { return handler(request.content) }
        PushAlert.write(opened, into: content)
        handler(content)
    }
}
