import SwiftUI

/// The message bubble's long-press menu: **Copy, Reply, Forward, Delete**
/// (Jacob, #1370; the emoji reaction row is gone, not moved). Forward opens
/// the same chat picker the media viewer uses. SwiftUI places a `.contextMenu`
/// itself, always on screen, so only the item set lives here.
enum MessageMenuItem: CaseIterable, Equatable {
    case copy
    case reply
    case forward
    case delete

    var label: String {
        switch self {
        case .copy: return "Copy"
        case .reply: return "Reply"
        case .forward: return "Forward"
        case .delete: return "Delete"
        }
    }

    var systemImage: String {
        switch self {
        case .copy: return "doc.on.doc"
        case .reply: return "arrowshape.turn.up.left"
        case .forward: return "arrowshape.turn.up.right"
        case .delete: return "trash"
        }
    }

    var isDestructive: Bool { self == .delete }
}

enum MessageMenu {
    /// The items, in order, for every real message bubble.
    static let items: [MessageMenuItem] = [.copy, .reply, .forward, .delete]

    /// Rows that are not a message in a shared doc (channels, date / system
    /// lines) can only be copied.
    static let copyOnly: [MessageMenuItem] = [.copy]

    static func items(forMessage: Bool) -> [MessageMenuItem] {
        forMessage ? items : copyOnly
    }

    /// The items for one bubble: Delete only when the relay would let this user delete it
    /// (`MessageDeleteRule.mayDelete`) — a member sees Copy / Reply / Forward on someone
    /// else's message, the owner sees all.
    static func items(forMessage: Bool, canDelete: Bool) -> [MessageMenuItem] {
        guard forMessage else { return copyOnly }
        return canDelete ? items : items.filter { !$0.isDestructive }
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

/// The menu's buttons, for `.contextMenu { MessageMenuContent(...) }`.
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
