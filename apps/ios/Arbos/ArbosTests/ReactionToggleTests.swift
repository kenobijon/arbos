import XCTest
@testable import Arbos

/// Swift `toggleReaction` must match core `set_reaction` for flat keys.
/// Legacy nested emoji buckets are covered by crates/arbos-ffi
/// `toggle_reaction_flat_twice_and_legacy_once_clear`.
final class ReactionToggleTests: XCTestCase {
    func testUnReactClearsFlatKey() throws {
        let doc = CoreChatDoc(key: "react-flat")
        try doc.insertUserMessage(id: "m1", text: "hi", deviceId: "dev", author: .init(sender: "user:ken"))
        try doc.toggleReaction(messageId: "m1", emoji: "👍", sender: "user:ken")
        doc.commit()
        XCTAssertEqual(doc.readAll().first?.reactions.first?.emoji, "👍")

        try doc.toggleReaction(messageId: "m1", emoji: "👍", sender: "user:ken")
        doc.commit()
        XCTAssertTrue(doc.readAll().first?.reactions.isEmpty == true)
    }

    func testToggleOnThenOffMatchesCoreFlatShape() throws {
        let doc = CoreChatDoc(key: "react-roundtrip")
        try doc.insertUserMessage(id: "m2", text: "hi", deviceId: "dev", author: .init(sender: "user:ken"))
        try doc.toggleReaction(messageId: "m2", emoji: "❤️", sender: "user:ken")
        doc.commit()
        XCTAssertEqual(doc.readAll().first?.reactions.map(\.emoji), ["❤️"])
        try doc.toggleReaction(messageId: "m2", emoji: "❤️", sender: "user:ken")
        doc.commit()
        XCTAssertTrue(doc.readAll().first?.reactions.isEmpty == true)
    }
}
