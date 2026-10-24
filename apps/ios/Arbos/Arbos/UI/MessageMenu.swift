import SwiftUI

/// The message bubble's long-press menu: **Copy, Reply, Forward, Reply in DM,
/// Edit (own messages), Delete** (#1370 + #1472; Edit is own-message only).
/// Forward opens the same chat picker the media viewer uses; Reply in DM opens a
/// private chat with the human who wrote the message. SwiftUI places a
/// `.contextMenu` itself, always on screen, so only the item set lives here.
enum MessageMenuItem: CaseIterable, Equatable {
    case copy
    case reply
    case forward
    case replyInDM
    case edit
    case delete

    var label: String {
        switch self {
        case .copy: return "Copy"
        case .reply: return "Reply"
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
        case .forward: return "arrowshape.turn.up.right"
        case .replyInDM: return "bubble.left"
        case .edit: return "pencil"
        case .delete: return "trash"
        }
    }

    var isDestructive: Bool { self == .delete }
}

enum MessageMenu {
    /// The items, in order, for every real message bubble (Edit is added only
    /// for the user's own text via `items(forMessage:canEdit:)`).
    static let items: [MessageMenuItem] = [.copy, .reply, .forward, .replyInDM, .delete]

    /// Rows that are not a message in a shared doc (channels, date / system
    /// lines) can only be copied.
    static let copyOnly: [MessageMenuItem] = [.copy]

    static func items(forMessage: Bool, canEdit: Bool = false) -> [MessageMenuItem] {
        guard forMessage else { return copyOnly }
        var out: [MessageMenuItem] = [.copy, .reply, .forward, .replyInDM]
        if canEdit { out.append(.edit) }
        out.append(.delete)
        return out
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
