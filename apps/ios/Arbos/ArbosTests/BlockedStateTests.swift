import XCTest
@testable import Arbos

/// The Blocked state ([feedback blocked-state-ios-2026-10-26]; the Mac's `blocked::tests` /
/// `push_refusal_tests` twin): the words, the block-list match, the refusal mapping, and the
/// structure — every surface draws the shared tag / chip / bar and every Unblock asks first.
final class BlockedStateTests: XCTestCase {
    private func person(_ id: String, _ handle: String, _ name: String? = nil) -> Person {
        Person(id: id, handle: handle, avatarUrl: nil, name: name, status: "member")
    }

    func testTheWords() {
        XCTAssertEqual(BlockedState.label, "Blocked")
        XCTAssertEqual(BlockedState.unblock, "Unblock")
        XCTAssertEqual(BlockedState.confirmTitle(handle: "sam"), "Unblock @sam?")
        XCTAssertEqual(BlockedState.composerLine(name: "Sam Rivera"), "You blocked Sam Rivera")
        XCTAssertFalse(BlockedState.confirmNote.isEmpty)
    }

    func testBlockedMatchesByIdThenHandle() {
        let list = [person("gh_2", "sam", "Sam Rivera")]
        XCTAssertTrue(BlockedState.isBlocked(id: "gh_2", handle: "someone-else", in: list))
        XCTAssertTrue(BlockedState.isBlocked(id: "", handle: "Sam", in: list))
        XCTAssertFalse(BlockedState.isBlocked(id: "gh_3", handle: "priya", in: list))
        XCTAssertFalse(BlockedState.isBlocked(id: "gh_2", handle: "sam", in: []))
    }

    func testTheRefusalSentenceFollowsTheRelaysReason() {
        XCTAssertEqual(
            BlockedState.refusalSentence(reason: "blocked_by", name: "Sam Rivera", handle: "sam"),
            "Sam Rivera has blocked you in this chat."
        )
        XCTAssertEqual(
            BlockedState.refusalSentence(reason: "blocked_by", name: nil, handle: "sam"),
            "@sam has blocked you in this chat."
        )
        XCTAssertEqual(
            BlockedState.refusalSentence(reason: "blocked_by", name: " ", handle: ""),
            "Someone in this chat has blocked you."
        )
        XCTAssertEqual(
            BlockedState.refusalSentence(reason: "blocks_member", name: "Sam Rivera", handle: "sam"),
            "Couldn't post: you've blocked Sam Rivera, who is in this chat. Unblock to post."
        )
        XCTAssertEqual(
            BlockedState.refusalSentence(reason: "silenced", name: nil, handle: nil),
            "You're muted in this chat, so your messages aren't being delivered."
        )
        for reason in [nil, "chat_unavailable", "something_new"] {
            XCTAssertEqual(
                BlockedState.refusalSentence(reason: reason, name: nil, handle: nil),
                "Messages in this chat aren't being delivered right now. We'll keep trying.",
                "an old relay / an unknown reason keeps the generic line"
            )
        }
        XCTAssertTrue(BlockedState.offersUnblock(reason: "blocks_member"))
        XCTAssertFalse(BlockedState.offersUnblock(reason: "blocked_by"))
        XCTAssertFalse(BlockedState.offersUnblock(reason: nil))
        let why = PushRefusalWhy(code: "not_available", reason: "blocked_by", byHandle: "sam", byName: "Sam Rivera")
        XCTAssertEqual(why.sentence, "Sam Rivera has blocked you in this chat.")
    }

    // MARK: - Structure

    private var sourceRoot: URL {
        // …/apps/ios/Arbos/ArbosTests/BlockedStateTests.swift → …/apps/ios/Arbos/Arbos
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("Arbos")
    }

    private func source(_ rel: String) throws -> String? {
        let url = sourceRoot.appendingPathComponent(rel)
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        return try String(contentsOf: url, encoding: .utf8)
    }

    /// The person sheet: the chip under the @handle, Unblock as the hero action and the
    /// footer row, both through the one confirm; the member row carries the tag.
    func testThePersonSheetAndMemberRowsSayBlocked() throws {
        guard let pages = try source("UI/ProfilePages.swift") else {
            throw XCTSkip("sources not beside the test bundle (CI without the tree)")
        }
        XCTAssertTrue(pages.contains("subtitleBelow: blockedEntry != nil && !isMe ? AnyView(BlockedChip()) : nil"), "the chip under the @handle")
        XCTAssertTrue(pages.contains("? AnyView(unblockButton)"), "Unblock is the hero action once blocked")
        XCTAssertTrue(pages.contains("PanePillButton(glyph: \"nosign\", title: BlockedState.unblock) { confirmUnblock = true }"), "the pill asks first")
        XCTAssertTrue(pages.contains("PaneActionRow(title: BlockRow.title(blocked: true), color: theme.accent) { confirmUnblock = true }"), "the row asks first")
        XCTAssertFalse(pages.contains("color: theme.accent) { model.unblock(entry) }"), "no unblock without the confirm")
        XCTAssertTrue(pages.contains(".confirmationDialog(BlockedState.confirmTitle(handle: person.handle), isPresented: $confirmUnblock"), "one confirm for both")
        XCTAssertTrue(pages.contains("case (true, let manage?): return AnyView(HStack(spacing: 6) { BlockedTag(); manage })"), "the member row's tag before the menu")
        XCTAssertTrue(pages.contains("case (true, nil): return AnyView(BlockedTag())"))
    }

    /// The DM: the header pill's tag, the composer bar in the composer's place, the confirm.
    func testTheDmHeaderTagsAndTheComposerIsTheBlockedBar() throws {
        guard let chat = try source("UI/ChatView.swift") else {
            throw XCTSkip("sources not beside the test bundle (CI without the tree)")
        }
        XCTAssertTrue(chat.contains("if blockedPeer != nil { BlockedTag() }"), "the tag after the DM peer's name")
        XCTAssertTrue(chat.contains("} else if let peer = blockedPeer {"), "the bar takes the composer's branch")
        XCTAssertTrue(chat.contains("BlockedComposerBar(name: peer.displayName) { confirmUnblock = true }"), "the bar's Unblock asks first")
        XCTAssertTrue(chat.contains(".confirmationDialog(BlockedState.confirmTitle(handle: blockedPeer?.handle ?? counterpart?.handle ?? \"\"), isPresented: $confirmUnblock"))
        XCTAssertTrue(chat.contains("BlockedState.isBlocked(id: other.id, handle: other.handle, in: model.blocked)"), "from the relay's block list")
    }

    /// The refusal: the room keeps the relay's reason, the model shows its sentence.
    func testTheRefusalNoticeUsesTheRelaysReason() throws {
        guard let room = try source("Sync/SessionRoom.swift"),
              let model = try source("Store/AppModel.swift") else {
            throw XCTSkip("sources not beside the test bundle (CI without the tree)")
        }
        XCTAssertTrue(room.contains("case .pushRefused(_, let code, let reason, let byHandle, let byName):"), "the frame's fields are read")
        XCTAssertTrue(room.contains("lastRefusal = PushRefusalWhy(code: code, reason: reason, byHandle: byHandle, byName: byName)"))
        XCTAssertTrue(model.contains("let why = room?.lastRefusal"), "the model reads the reason")
        XCTAssertTrue(model.contains("let text = why?.sentence ?? BlockedState.refusalSentence(reason: nil, name: nil, handle: nil)"))
        XCTAssertFalse(model.contains("self.notice = Notice(text: \"Messages in this chat are not being delivered\")"), "no fixed sentence left")
    }
}
