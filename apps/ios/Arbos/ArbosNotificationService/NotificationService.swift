// Notification Service Extension: an ARBOS_MESSAGE push becomes a
// communication notification with the sender's picture (see
// Shared/MessageNotification.swift for the payload and the rendering).
// Anything else passes through untouched.

import UserNotifications

final class NotificationService: UNNotificationServiceExtension {
    private var handler: ((UNNotificationContent) -> Void)?
    private var content: UNMutableNotificationContent?

    override func didReceive(_ request: UNNotificationRequest, withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        handler = contentHandler
        let mutable = (request.content.mutableCopy() as? UNMutableNotificationContent) ?? UNMutableNotificationContent()
        content = mutable
        guard let push = MessagePush(userInfo: request.content.userInfo) else {
            contentHandler(mutable)
            return
        }
        Task { @MainActor in
            let updated = await push.makeContent(base: mutable)
            self.finish(updated)
        }
    }

    /// Out of time: deliver what we have (title/body are already set by the relay).
    override func serviceExtensionTimeWillExpire() {
        if let content { finish(content) }
    }

    private func finish(_ c: UNNotificationContent) {
        guard let h = handler else { return }
        handler = nil
        h(c)
    }
}
