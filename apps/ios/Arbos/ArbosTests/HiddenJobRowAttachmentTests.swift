import XCTest
@testable import Arbos

/// Jacob, 2026-10-27 16:05 UTC (Arbonites, Doug agent on agentd 0.1.150): the SN120 chart PNG was
/// delivered onto the job's hidden worker row (`job` + `hidden`) and the app drew nothing — "the
/// chart is in the card above" over a different message. A hidden/job row still draws its
/// attachment cards; everything else on it (status, text, the "no answer" line) stays hidden.
final class HiddenJobRowAttachmentTests: XCTestCase {
    private let chat = "01M5MHDEZFB5SS6VRK9T12NVAC"
    private let t0: Int64 = 1_700_000_000_000

    private func jobRow(withAttachment: Bool) -> [String: Any] {
        var parts: [[String: Any]] = [
            ["id": "s", "kind": "status", "code": "working", "message": "Plotting SN120 price"],
            ["id": "t", "kind": "text", "text": "Done."],
        ]
        if withAttachment {
            parts.append(["id": "att1", "kind": "attachment",
                          "attachment": ["name": "sn120_price.png", "mime": "image/png", "ref": "7ba1fc21",
                                         "bytes": 20_260, "kind": "image", "title": "SN120 price"]])
        }
        return ["id": "j1", "role": "assistant", "sender": "agent:doug", "senderHandle": "doug-agent", "status": "done",
                "createdAt": t0 + 1_000, "hidden": true,
                "job": ["id": "01M5Z1QCTG61DZ98MRAWH95FBF", "title": "Running the program", "kind": "code"],
                "parts": parts]
    }

    func testAHiddenJobRowStillDrawsItsAttachmentCard() throws {
        let m = try XCTUnwrap(DocJSON.message(jobRow(withAttachment: true), chatId: chat))
        XCTAssertTrue(m.hidden)
        XCTAssertNotNil(m.job)
        let rows = Transcript.rows([m], projectId: "doug", people: true)
        let mine = rows.filter { $0.messageId == "j1" }
        XCTAssertEqual(mine.map(\.id), ["j1#att1"], "exactly the card, nothing else: \(rows.map(\.id))")
        guard case .image(let item, let fromUser, _, let lane) = mine[0].kind else {
            return XCTFail("an image card: \(mine[0].kind)")
        }
        XCTAssertEqual(item.name, "sn120_price.png")
        XCTAssertEqual(item.slot, "doug")
        XCTAssertFalse(fromUser)
        XCTAssertTrue(lane, "agent media in a people chat keeps the sender lane")
    }

    func testAHiddenJobRowWithoutAFileStillDrawsNothing() throws {
        let m = try XCTUnwrap(DocJSON.message(jobRow(withAttachment: false), chatId: chat))
        let rows = Transcript.rows([m], projectId: "doug", people: true)
        XCTAssertFalse(rows.contains { $0.messageId == "j1" }, "wordless by design: \(rows.map(\.id))")
    }
}
