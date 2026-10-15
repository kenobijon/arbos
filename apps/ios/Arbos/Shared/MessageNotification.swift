// A message notification, Grok Bot style: the sender's picture (an agent's
// tree in its colour from the bundled v2 assets, or a person's GitHub
// avatar) on the banner and lock screen, the preview as the body. Shared by
// the app (local notifications from the /me/pushes poll) and the
// Notification Service Extension (remote pushes).
//
// Payload (relay → APNs, `mutable-content: 1`):
//   aps: { alert: {title, body}, badge, sound, "thread-id": <chat_id>, category: "ARBOS_MESSAGE" }
//   chat_id, project_id ("people" for DMs), message_id, chat_title (groups),
//   sender: { kind: "agent"|"user", id, name, handle, avatar, avatar_color, avatar_url }
// Flat `sender_*` keys are accepted too.

import UserNotifications
import Intents
import UIKit
import os

let messageCategory = "ARBOS_MESSAGE"
private let log = Logger(subsystem: "com.unarbos.arbos.ios", category: "message-notification")

struct MessagePush {
    let chatId: String
    let projectId: String
    let title: String
    let body: String
    let senderKind: String
    let senderId: String
    let senderName: String
    let senderHandle: String
    let avatarSlug: String?
    let avatarColor: String?
    let avatarUrl: String?
    let groupName: String?

    init?(userInfo: [AnyHashable: Any]) {
        let aps = userInfo["aps"] as? [String: Any] ?? [:]
        let category = (aps["category"] as? String) ?? (userInfo["category"] as? String) ?? ""
        guard category == messageCategory, let chat = userInfo["chat_id"] as? String, !chat.isEmpty else { return nil }
        let alert = aps["alert"] as? [String: Any] ?? [:]
        let sender = userInfo["sender"] as? [String: Any] ?? [:]
        func s(_ nested: String, _ flat: String) -> String? {
            let v = (sender[nested] as? String) ?? (userInfo[flat] as? String)
            return (v?.isEmpty == false) ? v : nil
        }
        chatId = chat
        projectId = (userInfo["project_id"] as? String) ?? "people"
        title = (alert["title"] as? String) ?? ""
        body = (alert["body"] as? String) ?? (aps["alert"] as? String) ?? ""
        senderKind = s("kind", "sender_kind") ?? (s("avatar", "sender_avatar") != nil ? "agent" : "user")
        senderId = s("id", "sender_id") ?? s("handle", "sender_handle") ?? chat
        senderHandle = s("handle", "sender_handle") ?? ""
        senderName = s("name", "sender_name") ?? (senderHandle.isEmpty ? title : senderHandle)
        avatarSlug = s("avatar", "sender_avatar")
        avatarColor = s("avatar_color", "sender_avatar_color")
        avatarUrl = s("avatar_url", "sender_avatar_url")
        groupName = (userInfo["chat_title"] as? String).flatMap { $0.isEmpty ? nil : $0 }
    }

    /// The same shape built locally (the /me/pushes poll, the debug hook).
    static func userInfo(chatId: String, projectId: String, title: String, body: String, senderKind: String, senderId: String,
                         senderName: String, senderHandle: String, avatar: String?, avatarColor: String?, avatarUrl: String?,
                         chatTitle: String? = nil) -> [AnyHashable: Any] {
        var sender: [String: Any] = ["kind": senderKind, "id": senderId, "name": senderName, "handle": senderHandle]
        if let avatar { sender["avatar"] = avatar }
        if let avatarColor { sender["avatar_color"] = avatarColor }
        if let avatarUrl { sender["avatar_url"] = avatarUrl }
        var info: [AnyHashable: Any] = [
            "aps": ["alert": ["title": title, "body": body], "category": messageCategory, "thread-id": chatId] as [String: Any],
            "chat_id": chatId, "project_id": projectId, "sender": sender,
        ]
        if let chatTitle { info["chat_title"] = chatTitle }
        return info
    }

    /// The agent's tree (tinted) or the person's GitHub picture.
    @MainActor
    func senderImage() async -> INImage? {
        if senderKind == "agent", let slug = avatarSlug,
           let png = AvatarImage.treePNG(slug: slug, colorName: avatarColor, pixels: 192) {
            return INImage(imageData: png)
        }
        if let urlString = avatarUrl, let url = URL(string: urlString) {
            var req = URLRequest(url: url)
            req.timeoutInterval = 6
            if let (data, _) = try? await URLSession.shared.data(for: req), !data.isEmpty,
               let round = AvatarImage.roundPNG(data, pixels: 192) {
                return INImage(imageData: round)
            }
        }
        if let slug = avatarSlug, let png = AvatarImage.treePNG(slug: slug, colorName: avatarColor, pixels: 192) {
            return INImage(imageData: png)
        }
        return nil
    }

    /// Donates an INSendMessageIntent and returns the content the system
    /// draws with the sender's picture; nil when the intent is refused
    /// (missing entitlement), so callers keep the plain content.
    func communicationContent(from content: UNMutableNotificationContent, image: INImage?) -> UNNotificationContent? {
        let handle = INPersonHandle(value: senderHandle.isEmpty ? senderId : senderHandle, type: .unknown)
        var components = PersonNameComponents()
        components.nickname = senderName
        let person = INPerson(personHandle: handle, nameComponents: components, displayName: senderName,
                              image: image, contactIdentifier: nil, customIdentifier: senderId, isMe: false, suggestionType: .none)
        let group = groupName.map { INSpeakableString(spokenPhrase: $0) }
        let intent = INSendMessageIntent(recipients: nil, outgoingMessageType: .outgoingMessageText, content: body,
                                         speakableGroupName: group, conversationIdentifier: chatId,
                                         serviceName: "Arbos", sender: person, attachments: nil)
        if let image {
            intent.setImage(image, forParameterNamed: \.sender)
            if group != nil { intent.setImage(image, forParameterNamed: \.speakableGroupName) }
        }
        let interaction = INInteraction(intent: intent, response: nil)
        interaction.direction = .incoming
        interaction.donate { error in if let error { log.error("donate: \(error.localizedDescription, privacy: .public)") } }
        do {
            return try content.updating(from: intent)
        } catch {
            log.error("updating(from:) failed: \(error.localizedDescription, privacy: .public)")
            return nil
        }
    }

    /// Plain content + the picture: what both the app and the extension deliver.
    @MainActor
    func makeContent(base: UNMutableNotificationContent) async -> UNNotificationContent {
        base.categoryIdentifier = messageCategory
        if base.threadIdentifier.isEmpty { base.threadIdentifier = chatId }
        if base.title.isEmpty { base.title = title }
        if base.body.isEmpty { base.body = body }
        var info = base.userInfo
        info["chat_id"] = chatId
        info["project_id"] = projectId
        base.userInfo = info
        let image = await senderImage()
        log.notice("message for chat \(chatId, privacy: .public) from \(senderName, privacy: .public) image=\(image == nil ? "none" : "ok", privacy: .public)")
        return communicationContent(from: base, image: image) ?? base
    }
}
