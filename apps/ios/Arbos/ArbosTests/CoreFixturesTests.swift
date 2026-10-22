import XCTest
import ArbosFFI
@testable import Arbos

/// Core golden fixtures under `crates/arbos-core/fixtures` (host path via `#filePath`).
final class CoreFixturesTests: XCTestCase {
    private static let repoRoot: URL = {
        // apps/ios/Arbos/ArbosTests/ThisFile.swift → repo root
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
    }()

    private static let fixturesDir: URL = {
        repoRoot.appendingPathComponent("crates/arbos-core/fixtures")
    }()

    private static let apiFixturesDir: URL = {
        repoRoot.appendingPathComponent("crates/arbos-relay/tests/fixtures/api")
    }()

    private func fixtureData(_ name: String) throws -> Data {
        let url = Self.fixturesDir.appendingPathComponent(name)
        return try Data(contentsOf: url)
    }

    private func apiFixtureData(_ name: String) throws -> Data {
        let url = Self.apiFixturesDir.appendingPathComponent(name)
        return try Data(contentsOf: url)
    }

    private func apiFixtureJSON(_ name: String) throws -> Any {
        try JSONSerialization.jsonObject(with: apiFixtureData(name))
    }

    private func expectedRows() throws -> [[String: Any]] {
        let data = try fixtureData("chat_doc_expected.json")
        let json = try JSONSerialization.jsonObject(with: data)
        guard let rows = json as? [[String: Any]] else {
            XCTFail("expected array"); return []
        }
        return rows
    }

    /// CoreChatDoc path: import the snapshot and compare shape.
    func testCoreChatDocReadsFixture() throws {
        let snap = try fixtureData("chat_doc.bin")
        let doc = CoreChatDoc(key: "chat-fixture")
        try doc.importUpdate(snap)
        let messages = doc.readAll()
        let expected = try expectedRows()
        XCTAssertEqual(messages.count, expected.count)

        for (i, exp) in expected.enumerated() {
            let msg = messages[i]
            XCTAssertEqual(msg.id, exp["id"] as? String, "row \(i) id")
            XCTAssertEqual(msg.role.rawValue, exp["role"] as? String, "row \(i) role")
            XCTAssertEqual(msg.plainText, exp["text"] as? String ?? "", "row \(i) text")
            XCTAssertEqual(msg.hiddenCount, exp["hiddenCount"] as? Int ?? 0, "row \(i) hiddenCount")
            if let greeting = exp["greeting"] as? Bool {
                XCTAssertEqual(msg.greeting, greeting, "row \(i) greeting")
            }
            if let qid = exp["answersQuestionId"] as? String {
                // answersQuestionId is not a Message field; presence is enough via id/text.
                XCTAssertFalse(qid.isEmpty)
            }
            if let reactions = exp["reactions"] as? [String: Any], !reactions.isEmpty {
                XCTAssertFalse(msg.reactions.isEmpty, "row \(i) reactions")
                for (emoji, people) in reactions {
                    guard let people = people as? [String: Any] else { continue }
                    let got = msg.reactions.first { $0.emoji == emoji }
                    XCTAssertNotNil(got, "row \(i) missing emoji \(emoji)")
                    for (sender, flag) in people {
                        guard (flag as? Bool) == true else { continue }
                        XCTAssertTrue(got?.senders.contains(sender) == true, "row \(i) \(emoji)/\(sender)")
                    }
                }
            }
        }
        XCTAssertEqual(messages[1].hiddenCount, 1, "assistant fixture must expose one hidden tool part")
    }

    /// arbos-ffi path: same snapshot through UniFFI ChatDoc.readRowsJson.
    func testFfiReadsCoreChatDocFixture() throws {
        let snap = try fixtureData("chat_doc.bin")
        let doc = try ArbosFFI.ChatDoc.openFromSnapshot(snapshot: snap)
        let raw = doc.readRowsJson()
        let json = try JSONSerialization.jsonObject(with: Data(raw.utf8))
        guard let rows = json as? [[String: Any]] else {
            return XCTFail("FFI rows not an array")
        }
        let expected = try expectedRows()
        XCTAssertEqual(rows.count, expected.count)

        for (i, exp) in expected.enumerated() {
            let row = rows[i]
            XCTAssertEqual(row["id"] as? String, exp["id"] as? String, "row \(i) id")
            XCTAssertEqual(row["role"] as? String, exp["role"] as? String, "row \(i) role")
            let text = Self.messageText(row)
            XCTAssertEqual(text, exp["text"] as? String ?? "", "row \(i) text")
            let hidden = (row["hiddenParts"] as? [Any])?.count ?? 0
            XCTAssertEqual(hidden, exp["hiddenCount"] as? Int ?? 0, "row \(i) hiddenCount")
            if let reactions = exp["reactions"] as? [String: Any] {
                let got = row["reactions"] as? [String: Any] ?? [:]
                XCTAssertEqual(NSDictionary(dictionary: got), NSDictionary(dictionary: reactions), "row \(i) reactions")
            }
        }
        let assistantHidden = (rows[1]["hiddenParts"] as? [Any])?.count ?? 0
        XCTAssertEqual(assistantHidden, 1)
    }

    func testLoroSwiftAndFfiDecodeFrameFixtures() throws {
        let cases: [(String, Arbos.FrameType, ArbosFFI.FrameType)] = [
            ("frame_hello.bin", .hello, .hello),
            ("frame_state.bin", .state, .state),
            ("frame_row.bin", .row, .row),
            ("frame_rows_done.bin", .rowsDone, .rowsDone),
            ("frame_push.bin", .push, .push),
            ("frame_ack.bin", .ack, .ack),
        ]
        for (name, swiftTy, ffiTy) in cases {
            let bytes = try fixtureData(name)
            let swiftFrame = Arbos.Frame.decode(bytes)
            XCTAssertEqual(swiftFrame?.type, swiftTy, name)
            let decoded = try ArbosFFI.decodeFrame(bytes: bytes)
            XCTAssertEqual(decoded.ty, ffiTy, name)
            XCTAssertEqual(decoded.payload, swiftFrame?.payload ?? Data(), name)
        }
    }

    private static func messageText(_ row: [String: Any]) -> String {
        let parts = row["parts"] as? [[String: Any]] ?? []
        return parts
            .filter { ($0["kind"] as? String) == "text" }
            .compactMap { $0["text"] as? String }
            .joined()
    }

    // MARK: Relay API DTO fixtures (step 6b-1)

    func testProjectDtoFixture() throws {
        let data = try apiFixtureData("project_json.json")
        let dto = try JSONDecoder().decode(ProjectDto.self, from: data)
        let project = Arbos.Project(dto: dto)
        XCTAssertEqual(project.id, "proj_fixture")
        XCTAssertEqual(project.name, "Fixture")
        XCTAssertEqual(project.state, .hibernatedLight)
        XCTAssertEqual(project.ownerHandle, "user101")
        XCTAssertEqual(project.primaryChatId, "chat_primary")
    }

    func testParseChatFixture() throws {
        let raw = try apiFixtureJSON("chat_json_for.json")
        let chat = try XCTUnwrap(RelayAPI.parseChat(raw))
        XCTAssertEqual(chat.id, "chat_fixture")
        XCTAssertEqual(chat.projectId, "proj_fixture")
        XCTAssertEqual(chat.title, "Hello")
    }

    func testPeopleChatParseFixtures() throws {
        let withMe = try XCTUnwrap(PeopleChat.parse(try apiFixtureJSON("people_chat_json_with_me.json")))
        XCTAssertEqual(withMe.members.count, 2)
        XCTAssertEqual(withMe.title, "user102")
        XCTAssertEqual(withMe.myRole, "owner")
        XCTAssertEqual(withMe.muted, false)

        let withoutMe = try XCTUnwrap(PeopleChat.parse(try apiFixtureJSON("people_chat_json_without_me.json")))
        XCTAssertEqual(withoutMe.members.count, 2)
        XCTAssertEqual(withoutMe.title, "user101, user102")
        XCTAssertEqual(withoutMe.myRole, "member")
        XCTAssertEqual(withoutMe.muted, false)
    }

    func testChatMemberParseFixtures() throws {
        let human = try XCTUnwrap(ChatMember.parse(try apiFixtureJSON("member_json_human.json")))
        XCTAssertEqual(human.handle, "user101")
        XCTAssertNil(human.project)

        let agent = try XCTUnwrap(ChatMember.parse(try apiFixtureJSON("member_json_agent.json")))
        XCTAssertEqual(agent.handle, "fixture")
        XCTAssertEqual(agent.project?.id, "proj_fixture")
    }

    func testProjectMembersFixture() throws {
        let data = try apiFixtureData("project_members_json.json")
        let dto = try JSONDecoder().decode(ProjectMembersDto.self, from: data)
        XCTAssertEqual(dto.members.count, 1)
        let members = dto.members.map { ProjectMember(dto: $0) }
        XCTAssertEqual(members.count, 1)
        XCTAssertEqual(members[0].id, "gh_101")
    }

    func testParseChatWithoutIdReturnsNil() throws {
        XCTAssertNil(RelayAPI.parseChat(["project_id": "proj_fixture", "title": "Nope"] as [String: Any]))
    }
}
