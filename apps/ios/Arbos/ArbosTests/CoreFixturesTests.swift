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

    // MARK: People / me / machine / unread (step 6b-2a)

    func testUserCardParseFixtures() throws {
        let legacy = try XCTUnwrap(Person.parse(try apiFixtureJSON("user_card_legacy.json")))
        XCTAssertEqual(legacy.id, "u_mac")
        XCTAssertEqual(legacy.handle, "jacobs-mac")
        XCTAssertEqual(legacy.name, "Jacob's Mac")

        let presence = try XCTUnwrap(Person.parse(try apiFixtureJSON("user_card_presence.json")))
        XCTAssertEqual(presence.handle, "user301")
        XCTAssertEqual(presence.online, false)
        XCTAssertEqual(presence.avatarColorIndex, 2)

        let github = try XCTUnwrap(Person.parse(try apiFixtureJSON("user_card_github.json")))
        XCTAssertEqual(github.status, "github")
        XCTAssertEqual(github.handle, "ghostuser")
    }

    func testMachineStatusFixture() throws {
        let data = try apiFixtureData("machine_status.json")
        let dto = try JSONDecoder().decode(MachineStatusDto.self, from: data)
        let machine = Machine(dto: dto)
        XCTAssertEqual(machine.state, .hibernatedLight)
        XCTAssertEqual(machine.cpu, 4)
        XCTAssertEqual(machine.memGib, 8)
        XCTAssertEqual(machine.workspaces["proj_fixture"], .idle)
        XCTAssertEqual(dto.user_id, "gh_301")
    }

    func testUnreadFixture() throws {
        let data = try apiFixtureData("unread.json")
        let dto = try JSONDecoder().decode(UnreadDto.self, from: data)
        XCTAssertEqual(dto.total, 2)
        XCTAssertEqual(dto.chats.count, 1)
        XCTAssertEqual(dto.chats[0].chat_id, "people_dm")
        XCTAssertEqual(dto.chats[0].unread, 2)
    }

    // MARK: Calls (step 6b-2b)

    func testCallParseFixtures() throws {
        let live = try XCTUnwrap(Call.parse(try apiFixtureJSON("call_live_with_token.json")))
        XCTAssertEqual(live.callId, "call_fixture")
        XCTAssertEqual(live.chatId, "people_call_fixture")
        XCTAssertEqual(live.token, "fixture.join.token")
        XCTAssertEqual(live.startedBy?.handle, "user201")
        XCTAssertEqual(live.participants.count, 1)

        let ended = try XCTUnwrap(Call.parse(try apiFixtureJSON("call_ended.json")))
        XCTAssertEqual(ended.callId, "call_fixture")
        XCTAssertNotNil(ended.endedAt)
        XCTAssertEqual(ended.reason, "last_left")
        XCTAssertNil(ended.token)

        let joinEnv = try apiFixtureData("join_call_response.json")
        let join = try JSONDecoder().decode(CallEnvelopeDto.self, from: joinEnv)
        let joined = Call(dto: join.call)
        XCTAssertEqual(joined.callId, "call_fixture")
        XCTAssertEqual(joined.token, "fixture.join.token")
    }

    func testStartCallResponseFixture() throws {
        let data = try apiFixtureData("start_call_response.json")
        let resp = try JSONDecoder().decode(StartCallResponseDto.self, from: data)
        XCTAssertEqual(resp.joined, false)
        XCTAssertEqual(resp.reachable?.count, 1)
        let call = Call(dto: resp.call, reachable: (resp.reachable ?? []).map(CallReachability.init(dto:)))
        XCTAssertEqual(call.callId, "call_fixture")
        XCTAssertEqual(call.reachable.count, 1)
        XCTAssertEqual(call.reachable[0].userId, "gh_202")
        XCTAssertEqual(call.reachable[0].anyVoip, true)
    }

    func testLeaveAndVoipResponseFixtures() throws {
        let leave = try JSONDecoder().decode(LeaveCallResponseDto.self, from: try apiFixtureData("leave_call_response.json"))
        XCTAssertEqual(leave.ended, true)
        XCTAssertEqual(leave.duration_s, 60)
        XCTAssertNil(leave.already_ended)
    }

    func testCallPayloadFixtures() throws {
        let started = try JSONDecoder().decode(CallStartedPayloadDto.self, from: try apiFixtureData("call_started_payload.json"))
        XCTAssertEqual(started.call.call_id, "call_fixture")
        XCTAssertEqual(started.sender.handle, "user201")
        XCTAssertEqual(Call(dto: started.call).callId, "call_fixture")

        let ended = try JSONDecoder().decode(CallEndedPayloadDto.self, from: try apiFixtureData("call_ended_payload.json"))
        XCTAssertEqual(ended.call_id, "call_fixture")
        XCTAssertEqual(ended.writer_device, "dev_bob_call")

        let missed = try JSONDecoder().decode(CallMissedPayloadDto.self, from: try apiFixtureData("call_missed_payload.json"))
        XCTAssertEqual(missed.from.handle, "user201")
        XCTAssertEqual(CallPerson(dto: missed.from).handle, "user201")

        let ring = try JSONDecoder().decode(CallRingPayloadDto.self, from: try apiFixtureData("call_ring_payload.json"))
        XCTAssertEqual(ring.caller.handle, "user201")
        XCTAssertEqual(ring.room, "call_room_fixture")

        let alert = try JSONDecoder().decode(CallAlertDto.self, from: try apiFixtureData("call_alert.json"))
        XCTAssertEqual(alert.event, "started")
        XCTAssertEqual(alert.call.call_id, "call_fixture")
        XCTAssertEqual(alert.sender?.handle, "user201")
    }

    func testCallPersonMetadataFixture() throws {
        let data = try apiFixtureData("call_live_with_token.json")
        let dto = try JSONDecoder().decode(CallDto.self, from: data)
        let meta = try JSONEncoder().encode(dto.started_by)
        let person = try XCTUnwrap(CallPerson.parse(metadata: String(data: meta, encoding: .utf8)))
        XCTAssertEqual(person.id, "gh_201")
        XCTAssertEqual(person.handle, "user201")
    }

    // MARK: Goals (step 6b-2c-1)

    func testGoalParseFixtures() throws {
        let raw = try apiFixtureJSON("goal_json.json")
        let goal = try XCTUnwrap(GoalItem.parse(raw))
        XCTAssertEqual(goal.id, "goal_fixture")
        XCTAssertEqual(goal.projectId, "proj_fixture")
        XCTAssertEqual(goal.projectName, "Fixture")
        XCTAssertEqual(goal.title, "Ship the DTO step")
        XCTAssertEqual(goal.status, "active")
        XCTAssertEqual(goal.autonomy, "suggest")
        XCTAssertEqual(goal.plan.count, 2)
        XCTAssertEqual(goal.plan[0].title, "Add GoalDto")
        XCTAssertEqual(goal.plan[0].status, "done")
        XCTAssertEqual(goal.plan[1].status, "doing")
        XCTAssertEqual(goal.progressLog.count, 1)
        XCTAssertEqual(goal.progressLog[0].text, "Started the shared client core step.")
        XCTAssertEqual(goal.linkedChatIds, ["chat_primary"])
        XCTAssertEqual(goal.checkInCron, "0 9 * * 1-5")

        let listData = try apiFixtureData("goals_list.json")
        let list = try JSONDecoder().decode(GoalsListDto.self, from: listData)
        XCTAssertEqual(list.goals.count, 1)
        XCTAssertEqual(list.goals[0].id, "goal_fixture")
        let rawList = try apiFixtureJSON("goals_list.json") as? [String: Any]
        let first = try XCTUnwrap((rawList?["goals"] as? [Any])?.first)
        let listed = try XCTUnwrap(GoalItem.parse(first))
        XCTAssertEqual(listed.id, "goal_fixture")
        XCTAssertEqual(listed.plan.count, 2)
        XCTAssertEqual(listed.progressLog.count, 1)
    }
}
