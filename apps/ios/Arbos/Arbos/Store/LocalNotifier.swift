// Push-less notifications: the relay has no APNs yet, so `/me/pushes` is
// polled and surfaced as local notifications (banner in the foreground when
// the chat is not on screen, normal alert otherwise). Tapping one opens the
// chat through `openChatRequests`.

import Foundation
import UserNotifications

@MainActor
final class LocalNotifier: NSObject, UNUserNotificationCenterDelegate {
    static let shared = LocalNotifier()
    static let openChat = Notification.Name("arbos.openChat")
    private var asked = false

    func prepare() {
        UNUserNotificationCenter.current().delegate = self
        guard !asked else { return }
        asked = true
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in }
    }

    static func post(id: String, title: String, body: String, chatId: String) {
        shared.prepare()
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.userInfo = ["chat_id": chatId]
        content.threadIdentifier = chatId.isEmpty ? "arbos" : chatId
        let req = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(req) { _ in }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .sound, .list]
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let chatId = response.notification.request.content.userInfo["chat_id"] as? String ?? ""
        guard !chatId.isEmpty else { return }
        await MainActor.run {
            NotificationCenter.default.post(name: LocalNotifier.openChat, object: nil, userInfo: ["chat_id": chatId])
        }
    }
}
