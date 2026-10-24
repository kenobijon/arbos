import SwiftUI

/// The message bubble's menu under the reaction capsule (`MessageActions.swift`):
/// **Copy, Reply, View thread, Forward, Reply in DM, Edit (own messages), Delete**
/// (#1370 + #1472; View thread when the message has replies); reactions live in
/// the capsule, never here (Jacob, 2026-10-23). Forward opens the same chat
/// picker the media viewer uses; Reply in DM opens a private chat with the human
/// who wrote the message. Non-message rows get `copyOnly` as a plain `.contextMenu`.
enum MessageMenuItem: CaseIterable, Equatable {
    case copy
    case reply
    case viewThread
    case forward
    case replyInDM
    case edit
    case delete

    var label: String {
        switch self {
        case .copy: return "Copy"
        case .reply: return "Reply"
        case .viewThread: return "View thread"
        case .forward: return "Forward"
        case .replyInDM: return "Reply in DM"
        case .edit: return "Edit"
        case .delete: return "Delete"
        }
    }

    var systemImage: String {
        switch self {
        case .copy: return "doc.on.doc"
        case .reply: return "arrowshape.turn.up.left"
        case .viewThread: return "bubble.left.and.bubble.right"
        case .forward: return "arrowshape.turn.up.right"
        case .replyInDM: return "bubble.left"
        case .edit: return "pencil"
        case .delete: return "trash"
        }
    }

    var isDestructive: Bool { self == .delete }
}

enum MessageMenu {
    /// The items, in order, for every real message bubble (Edit / Delete / View
    /// thread are gated per row via `items(forMessage:canEdit:canDelete:hasReplies:)`).
    static let items: [MessageMenuItem] = [.copy, .reply, .forward, .replyInDM, .delete]

    /// Same as `items`, plus View thread after Reply when the message has replies
    /// (no Edit — that still comes from `canEdit` on the row).
    static let itemsWithThread: [MessageMenuItem] = [.copy, .reply, .viewThread, .forward, .replyInDM, .delete]

    /// Rows that are not a message in a shared doc (channels, date / system
    /// lines) can only be copied.
    static let copyOnly: [MessageMenuItem] = [.copy]

    static func items(forMessage: Bool) -> [MessageMenuItem] {
        items(forMessage: forMessage, canEdit: false, canDelete: true, hasReplies: false)
    }

    /// View thread when `hasReplies`; Edit only on the user's own text; Delete only
    /// when the relay would allow it (`MessageDeleteRule.mayDelete`).
    static func items(forMessage: Bool, canEdit: Bool = false, canDelete: Bool = true,
                      hasReplies: Bool = false) -> [MessageMenuItem] {
        guard forMessage else { return copyOnly }
        var out: [MessageMenuItem] = [.copy, .reply]
        if hasReplies { out.append(.viewThread) }
        out.append(contentsOf: [.forward, .replyInDM])
        if canEdit { out.append(.edit) }
        if canDelete { out.append(.delete) }
        return out
    }
}

/// Who may delete a message for everyone — the relay's rule (`arbos-relay/src/moderation.rs`),
/// mirrored so the menu never offers a Delete the relay would refuse: your own always; in a
/// people group the owner (and admins) delete anyone's; an agent's message is its owner's to
/// delete; in a project chat the owner deletes anything, a guest their own. System lines are a
/// moderator's only.
enum MessageDeleteRule {
    static func mayDelete(sender: String, meId: String?, moderates: Bool, agentOwnerIds: [String: String]) -> Bool {
        if moderates { return true }
        guard let meId else { return false }
        if sender == "user" || sender == "user:\(meId)" { return true }
        if sender.hasPrefix("agent:") {
            let pid = String(sender.dropFirst(6))
            return agentOwnerIds[pid] == meId
        }
        return false
    }
}

/// The menu's buttons, for `.contextMenu { MessageMenuContent(...) }` and the
/// long-press overlay under the reaction capsule.
struct MessageMenuContent: View {
    let items: [MessageMenuItem]
    let action: (MessageMenuItem) -> Void

    var body: some View {
        ForEach(items, id: \.self) { item in
            Button(role: item.isDestructive ? .destructive : nil) {
                action(item)
            } label: {
                Label(item.label, systemImage: item.systemImage)
            }
        }
    }
}
