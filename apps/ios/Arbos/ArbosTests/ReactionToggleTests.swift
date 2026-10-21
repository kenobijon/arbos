import XCTest
@testable import Arbos
import Loro

/// Swift `toggleReaction` must match core `set_reaction` for flat keys and
/// legacy nested emoji buckets (un-react on old rows).
final class ReactionToggleTests: XCTestCase {
    func testUnReactClearsFlatKey() throws {
        let doc = Arbos.ChatDoc(key: "react-flat")
        try doc.insertUserMessage(id: "m1", text: "hi", deviceId: "dev", author: .init(sender: "user:ken"))
        try doc.toggleReaction(messageId: "m1", emoji: "👍", sender: "user:ken")
        doc.commit()
        XCTAssertEqual(doc.readAll().first?.reactions.first?.emoji, "👍")

        try doc.toggleReaction(messageId: "m1", emoji: "👍", sender: "user:ken")
        doc.commit()
        XCTAssertTrue(doc.readAll().first?.reactions.isEmpty == true)
    }

    func testUnReactClearsLegacyNestedBucket() throws {
        let doc = Arbos.ChatDoc(key: "react-legacy")
        // Build a pre-flat-shape row: reactions → { "🎉": { "user:legacy": true } }.
        let list = doc.messages
        let map = try list.insertContainer(pos: list.len(), child: LoroMap())
        try map.insert(key: "id", v: "m-legacy")
        try map.insert(key: "role", v: "user")
        try map.insert(key: "sender", v: "user:legacy")
        try map.insert(key: "status", v: "done")
        try map.insert(key: "deviceId", v: "dev")
        try map.insert(key: "createdAt", v: nowMs())
        let reactions = try map.insertContainer(key: "reactions", child: LoroMap())
        let bucket = try reactions.insertContainer(key: "🎉", child: LoroMap())
        try bucket.insert(key: "user:legacy", v: true)
        let parts = try map.insertContainer(key: "parts", child: LoroList())
        let part = try parts.insertContainer(pos: parts.len(), child: LoroMap())
        try part.insert(key: "id", v: "p1")
        try part.insert(key: "kind", v: "text")
        let text = try part.insertContainer(key: "text", child: LoroText())
        try text.pushStr(s: "legacy")
        doc.commit()

        let before = doc.readAll()
        XCTAssertEqual(before.first?.reactions.first?.emoji, "🎉")
        XCTAssertEqual(before.first?.reactions.first?.senders, ["user:legacy"])

        try doc.toggleReaction(messageId: "m-legacy", emoji: "🎉", sender: "user:legacy")
        doc.commit()

        let after = doc.readAll()
        XCTAssertTrue(after.first?.reactions.isEmpty == true, "legacy bucket entry must clear on un-react")
    }

    func testToggleOnThenOffMatchesCoreFlatShape() throws {
        let doc = Arbos.ChatDoc(key: "react-roundtrip")
        try doc.insertUserMessage(id: "m2", text: "hi", deviceId: "dev", author: .init(sender: "user:ken"))
        try doc.toggleReaction(messageId: "m2", emoji: "❤️", sender: "user:ken")
        doc.commit()
        XCTAssertEqual(doc.readAll().first?.reactions.map(\.emoji), ["❤️"])
        try doc.toggleReaction(messageId: "m2", emoji: "❤️", sender: "user:ken")
        doc.commit()
        XCTAssertTrue(doc.readAll().first?.reactions.isEmpty == true)
    }
}
