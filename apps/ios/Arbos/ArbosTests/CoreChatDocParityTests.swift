import XCTest
@testable import Arbos
import ArbosFFI
import Loro

/// CoreChatDoc (ArbosFFI / arbos-core) must agree with ChatDoc (loro-swift)
/// on what the app reads — not on raw JSON shape.
final class CoreChatDocParityTests: XCTestCase {
    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("crates/arbos-core/fixtures")
    }()

    private func fixtureData(_ name: String) throws -> Data {
        try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
    }

    // MARK: - Helpers

    private func attachmentNames(_ m: Message) -> [String] {
        m.parts.compactMap { part in
            if case .attachment(_, let a) = part { return a.name }
            return nil
        }
    }

    private func assertMessagesEqual(
        _ a: [Message], _ b: [Message], _ label: String, file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertEqual(a.count, b.count, "\(label) count", file: file, line: line)
        for (i, (lhs, rhs)) in zip(a, b).enumerated() {
            let row = "\(label)[\(i)]"
            XCTAssertEqual(lhs.id, rhs.id, "\(row) id", file: file, line: line)
            XCTAssertEqual(lhs.role, rhs.role, "\(row) role", file: file, line: line)
            XCTAssertEqual(lhs.sender, rhs.sender, "\(row) sender", file: file, line: line)
            XCTAssertEqual(lhs.plainText, rhs.plainText, "\(row) plainText", file: file, line: line)
            XCTAssertEqual(attachmentNames(lhs), attachmentNames(rhs), "\(row) attachment names", file: file, line: line)
            XCTAssertEqual(lhs.mentions, rhs.mentions, "\(row) mentions", file: file, line: line)
            XCTAssertEqual(lhs.replyTo, rhs.replyTo, "\(row) replyTo", file: file, line: line)
            XCTAssertEqual(lhs.reactions, rhs.reactions, "\(row) reactions", file: file, line: line)
            XCTAssertEqual(lhs.hiddenCount, rhs.hiddenCount, "\(row) hiddenCount", file: file, line: line)
            XCTAssertEqual(lhs.status, rhs.status, "\(row) status", file: file, line: line)
        }
    }

    private func assertModelsEqual(
        _ a: [String: Any], _ b: [String: Any], file: StaticString = #filePath, line: UInt = #line
    ) {
        let am = a["models"] as? [String: Any]
        let bm = b["models"] as? [String: Any]
        let at = am?["talking_model"] as? [String: Any]
        let bt = bm?["talking_model"] as? [String: Any]
        XCTAssertEqual(at?["id"] as? String, bt?["id"] as? String, "models.talking_model.id", file: file, line: line)
        XCTAssertEqual(at?["effort"] as? String, bt?["effort"] as? String, "models.talking_model.effort", file: file, line: line)
    }

    private func seedWriters(into loro: ChatDoc, core: CoreChatDoc) throws {
        let author = ChatDoc.Author(sender: "user:ken", handle: "ken", name: "Ken", avatar: nil)
        let att = Attachment(name: "photo.png", mime: "image/png", blobRef: "blob-1", kind: "image")
        try loro.insertUserMessage(
            id: "m-user", text: "hello @bob", deviceId: "dev",
            attachments: [att], author: author, mentions: ["user:bob"], replyTo: "m-prev"
        )
        try core.insertUserMessage(
            id: "m-user", text: "hello @bob", deviceId: "dev",
            attachments: [att], author: author, mentions: ["user:bob"], replyTo: "m-prev"
        )

        try loro.insertSystemLine(id: "m-sys", text: "Call ended · 1 min", deviceId: "dev", callId: "call-1")
        try core.insertSystemLine(id: "m-sys", text: "Call ended · 1 min", deviceId: "dev", callId: "call-1")

        try loro.insertAssistantGreeting(
            id: "m-greet", text: "Hi, I'm Agent.", deviceId: "dev",
            projectId: "proj-1", agentName: "Agent"
        )
        try core.insertAssistantGreeting(
            id: "m-greet", text: "Hi, I'm Agent.", deviceId: "dev",
            projectId: "proj-1", agentName: "Agent"
        )

        try loro.toggleReaction(messageId: "m-user", emoji: "👍", sender: "user:ken")
        try core.toggleReaction(messageId: "m-user", emoji: "👍", sender: "user:ken")
        try loro.toggleReaction(messageId: "m-user", emoji: "👍", sender: "user:ken")
        try core.toggleReaction(messageId: "m-user", emoji: "👍", sender: "user:ken")

        try loro.setModelSettings(model: "gpt-test", effort: .high)
        try core.setModelSettings(model: "gpt-test", effort: .high)

        loro.commit()
        core.commit()
    }

    // MARK: - Tests

    func testWritersReadAllAndModelsAgree() throws {
        let loro = ChatDoc(key: "parity-writers")
        let core = CoreChatDoc(key: "parity-writers")
        try seedWriters(into: loro, core: core)
        assertMessagesEqual(loro.readAll(), core.readAll(), "writers")
        assertModelsEqual(loro.mapValue("settings"), core.mapValue("settings"))
    }

    func testFixtureChatDocBinReadAllAgrees() throws {
        let snap = try fixtureData("chat_doc.bin")
        let loro = ChatDoc(key: "chat-fixture")
        let core = CoreChatDoc(key: "chat-fixture")
        try loro.importUpdate(snap)
        try core.importUpdate(snap)
        assertMessagesEqual(loro.readAll(), core.readAll(), "fixture")
    }

    func testCrossImportSnapshotsReadAllAgree() throws {
        let loro = ChatDoc(key: "cross-loro")
        let core = CoreChatDoc(key: "cross-core")
        try seedWriters(into: loro, core: core)

        let fromLoro = try loro.exportSnapshot()
        let coreLoaded = CoreChatDoc(key: "cross-loro")
        try coreLoaded.importUpdate(fromLoro)
        assertMessagesEqual(loro.readAll(), coreLoaded.readAll(), "loro→core")

        let fromCore = try core.exportSnapshot()
        let loroLoaded = ChatDoc(key: "cross-core")
        try loroLoaded.importUpdate(fromCore)
        assertMessagesEqual(core.readAll(), loroLoaded.readAll(), "core→loro")
    }

    func testLocalUpdateImportedIntoChatDocAgrees() throws {
        let core = CoreChatDoc(key: "local-upd")
        var updates: [Data] = []
        core.onLocalUpdate { updates.append($0) }

        let author = ChatDoc.Author(sender: "user:ken", handle: nil, name: nil, avatar: nil)
        try core.insertUserMessage(id: "m1", text: "ping", deviceId: "dev", author: author)
        try core.insertSystemLine(id: "m2", text: "note", deviceId: "dev")
        core.commit()

        XCTAssertFalse(updates.isEmpty, "onLocalUpdate must fire on commit")
        let loro = ChatDoc(key: "local-upd")
        for u in updates {
            try loro.importUpdate(u)
        }
        assertMessagesEqual(core.readAll(), loro.readAll(), "localUpdate→ChatDoc")
    }
}
