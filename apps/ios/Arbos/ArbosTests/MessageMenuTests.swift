import XCTest
@testable import Arbos

/// The bubble's long-press menu is Copy / Reply / Delete and nothing else
/// (#1370) — no emoji row, no Mention; a non-message row copies only.
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
}
