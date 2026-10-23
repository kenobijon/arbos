import XCTest
@testable import Arbos

/// The bubble's action items are Copy / Reply / Delete (#1370) — reactions
/// sit in a separate `MessageReactionBar`, not as menu items; a non-message
/// row copies only.
final class MessageMenuTests: XCTestCase {
    func testItemsAreExactlyCopyReplyDelete() {
        XCTAssertEqual(MessageMenu.items, [.copy, .reply, .delete])
        XCTAssertEqual(MessageMenu.items.map(\.label), ["Copy", "Reply", "Delete"])
        XCTAssertEqual(MessageMenu.items(forMessage: true), MessageMenu.items)
    }

    func testNoEmojiOrExtraItems() {
        for item in MessageMenuItem.allCases {
            XCTAssertTrue(item.label.unicodeScalars.allSatisfy { $0.isASCII }, "\(item) label carries non-ASCII text")
            XCTAssertFalse(MessageReaction.palette.contains(item.label))
        }
        XCTAssertEqual(MessageMenuItem.allCases.count, 3)
    }

    func testOnlyDeleteIsDestructive() {
        XCTAssertEqual(MessageMenuItem.allCases.filter(\.isDestructive), [.delete])
    }

    func testNonMessageRowsCopyOnly() {
        XCTAssertEqual(MessageMenu.items(forMessage: false), [.copy])
    }

    /// #1389: a long-press on any block of a bubble reaches the one menu, and
    /// its Copy is the whole message — every paragraph, the list, the code
    /// span — not the block under the finger. The fixture's "rules" reply is
    /// the shape of Jacob's bubble: five blocks, one text part.
    @MainActor func testCopyOfAMultiBlockBubbleIsTheWholeMessage() {
        let text = AppModel.longReply
        let blocks = Markdown.blocks(text)
        XCTAssertGreaterThanOrEqual(blocks.count, 5, "the fixture reply must span several blocks")
        XCTAssertTrue(blocks.contains { if case .bullets = $0 { return true } else { return false } })
        let m = Message(id: "m1", chatId: "c1", role: .assistant, sender: "agent", status: .done, turn: .reply, decision: nil,
                        subagentId: nil, subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: 0,
                        completedAt: 0, pending: false, parts: [.text(id: "m1-t", text: text)], hiddenCount: 0)
        XCTAssertEqual(m.plainText, text)
        for case .paragraph(let p) in blocks {
            XCTAssertTrue(m.plainText.contains(p), "copy misses paragraph: \(p)")
        }
    }
}
