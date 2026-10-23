import SwiftUI
import XCTest
@testable import Arbos

/// The presence row over the composer (typing-indicator-left-2026-10-23): one
/// line of text whatever the count, and one typing mark per handle per chat.
final class PresenceRowTests: XCTestCase {
    func testOneLineWhateverTheCount() {
        XCTAssertEqual(PresenceLine.sentence(typing: ["Sam"], working: []), "Sam is typing…")
        XCTAssertEqual(PresenceLine.sentence(typing: ["Sam", "Mira"], working: []), "Sam and Mira are typing…")
        XCTAssertEqual(PresenceLine.sentence(typing: ["Sam", "Mira", "Kai", "Ava"], working: []), "Sam, Mira and 2 others are typing…")
        XCTAssertEqual(PresenceLine.sentence(typing: [], working: ["Lium"]), "Lium is working")
        XCTAssertEqual(PresenceLine.sentence(typing: [], working: ["Lium", "Home"]), "Lium and Home are working")
        XCTAssertEqual(PresenceLine.sentence(typing: [], working: ["Lium", "Home", "Doug"]), "Lium, Home and Doug are working")
        XCTAssertEqual(PresenceLine.sentence(typing: ["Sam"], working: ["Lium"]), "Sam is typing… · Lium is working")
        XCTAssertEqual(PresenceLine.sentence(typing: [], working: []), "")
    }

    func testDetailAndQueueApplyToASingleWorkerOnly() {
        XCTAssertEqual(PresenceLine.sentence(typing: [], working: ["Lium"], detail: "Cloning RaoFoundation/subtensor"), "Cloning RaoFoundation/subtensor")
        XCTAssertEqual(PresenceLine.sentence(typing: [], working: ["Lium", "Home"], detail: "Cloning"), "Lium and Home are working")
        XCTAssertEqual(PresenceLine.sentence(typing: [], working: ["Lium"], queued: true), "Lium is waiting for the machine")
    }

    @MainActor
    func testTypingMarksAreOnePerHandleNewestFirstAndExpire() {
        let model = AppModel()
        model.noteTyping(["handle": "sam", "typing": true], chatId: "c1")
        model.noteTyping(["handle": "mira", "typing": true], chatId: "c1")
        model.noteTyping(["handle": "sam", "typing": true], chatId: "c1")   // refresh, not a second entry
        XCTAssertEqual(Set(model.typingHandles(in: "c1")), ["sam", "mira"])
        XCTAssertEqual(model.typingHandles(in: "c1").count, 2, "one mark per handle")
        XCTAssertEqual(model.typingHandles(in: "c2"), [], "per chat")
        model.noteTyping(["handle": "mira", "typing": false], chatId: "c1")
        XCTAssertEqual(model.typingHandles(in: "c1"), ["sam"])
        model.typing["c1"] = ["sam": nowMs() - 1]
        XCTAssertEqual(model.typingHandles(in: "c1"), [], "a stale mark is gone")
    }

    func testFirstNamesFromTwoParticipantsOn() {
        let pic = AnyView(EmptyView())
        let one = PresenceRow(participants: [PresenceParticipant(id: "a", name: "Sam Okafor", activity: .typing, picture: pic)])
        XCTAssertEqual(one.text, "Sam Okafor is typing…")
        let two = PresenceRow(participants: [
            PresenceParticipant(id: "a", name: "Mira Kaplan", activity: .typing, picture: pic),
            PresenceParticipant(id: "b", name: "Sam Okafor", activity: .typing, picture: pic),
        ])
        XCTAssertEqual(two.text, "Mira and Sam are typing…")
        XCTAssertEqual(PresenceLine.firstName("@sam"), "@sam")
    }

    func testPictureIsTheBubblePictureOnThePlusColumn() {
        // Jacob 15:15 / 15:20: the row's picture and the bubble's picture are one size at
        // one x, and that x is the composer's `+` column (keyboard up: 19 + 42 / 2 = 40);
        // the row's gap is the bubble row's gap, so the text starts where the body does.
        XCTAssertEqual(PresenceRow.avatar, Theme.memberAvatar)
        XCTAssertEqual(PresenceRow.inset, Theme.avatarColumnInset)
        XCTAssertEqual(PresenceRow.gap, Theme.memberAvatarGap)
        let center = 16 + Theme.avatarColumnInset + Theme.memberAvatar / 2
        let plusCenter = Theme.composerInsetKeyboard + Theme.plusButton / 2
        XCTAssertEqual(center, plusCenter, accuracy: 1)
        XCTAssertEqual(16 + Theme.memberLane, 67, "text and bubble body at Grok's 67 pt")
        XCTAssertEqual(PresenceRow.maxPictures, 3)
    }
}
