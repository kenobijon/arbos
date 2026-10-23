import SwiftUI

/// The message bubble's long-press menu: **Copy, Reply, Forward, Delete**,
/// plus an emoji reaction row on messages from other users (Discord /
/// Telegram / WhatsApp style). Forward opens the same chat picker the media
/// viewer uses. The Mac's `ui/message_menu.rs` keeps Copy / Reply / Delete
/// only (#1370); iOS restores the reaction bar here. SwiftUI places a
/// `.contextMenu` itself, always on screen, so only the item set and the
/// reaction `ControlGroup`s live here.
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
}

/// Horizontal emoji chips at the top of the long-press menu. `ControlGroup`
/// `.menu` style only lays out 3 side-by-side; more than 3 in one group wraps
/// into a broken vertical stack, so the palette is chunked by threes.
struct MessageReactionBar: View {
    let onPick: (String) -> Void

    var body: some View {
        let palette = MessageReaction.palette
        ControlGroup {
            ForEach(Array(palette.prefix(3)), id: \.self) { emoji in
                Button { onPick(emoji) } label: { Text(emoji) }
            }
        }
        ControlGroup {
            ForEach(Array(palette.dropFirst(3).prefix(3)), id: \.self) { emoji in
                Button { onPick(emoji) } label: { Text(emoji) }
            }
        }
        ControlGroup {
            ForEach(Array(palette.dropFirst(6)), id: \.self) { emoji in
                Button { onPick(emoji) } label: { Text(emoji) }
            }
        }
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
