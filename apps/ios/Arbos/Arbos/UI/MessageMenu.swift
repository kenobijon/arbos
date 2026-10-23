import SwiftUI

/// The message bubble's long-press menu: **Copy, Reply, Reply in DM, Delete**
/// (#1370 + #1472; the emoji reaction row is gone, not moved). The Mac's
/// `ui/message_menu.rs` carries the same Copy / Reply / Delete contract for its
/// right-click menu; Reply in DM is iPhone-only for now. SwiftUI places a
/// `.contextMenu` itself, always on screen, so only the item set lives here.
enum MessageMenuItem: CaseIterable, Equatable {
    case copy
    case reply
    case replyInDM
    case delete

    var label: String {
        switch self {
        case .copy: return "Copy"
        case .reply: return "Reply"
        case .replyInDM: return "Reply in DM"
        case .delete: return "Delete"
        }
    }

    var systemImage: String {
        switch self {
        case .copy: return "doc.on.doc"
        case .reply: return "arrowshape.turn.up.left"
        case .replyInDM: return "bubble.left"
        case .delete: return "trash"
        }
    }

    var isDestructive: Bool { self == .delete }
}

enum MessageMenu {
    /// The items, in order, for every real message bubble.
    static let items: [MessageMenuItem] = [.copy, .reply, .replyInDM, .delete]

    /// Rows that are not a message in a shared doc (channels, date / system
    /// lines) can only be copied.
    static let copyOnly: [MessageMenuItem] = [.copy]

    static func items(forMessage: Bool) -> [MessageMenuItem] {
        forMessage ? items : copyOnly
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
