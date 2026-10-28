import XCTest

/// Hold a bubble → the reaction capsule → its add-emoji cell → the emoji keyboard →
/// type 🔥: the overlay closes and ONE 🔥 chip sits under the bubble
/// ([feedback iphone-emoji-picker-reaction-2026-10-28]: the pick closed the overlay and
/// added nothing, because the keyboard's insert reached the hidden field twice and the
/// second report toggled the reaction off again). Runs the fixture app (`ARBOS_FIXTURE=1`,
/// Debug builds): no relay, deterministic rows.
///
/// A message bubble is `bubble:<messageId>`, the capsule `message-actions-capsule`, its
/// add-emoji cell is labelled "More emoji", a chip "<emoji> reaction, <count>".
/// `ARBOS_SHOT_DIR` (set) makes the test write a PNG of each step there — the rig's
/// before/after shots.
final class ReactionPickerUITests: XCTestCase {
    /// The shared Mac runs the gate and releases at the same time; waits are generous.
    private let appear: TimeInterval = 90
    private let settle: TimeInterval = 20

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = ["ARBOS_FIXTURE": "1", "ARBOS_OPEN_PROJECT": "fx-comms"]
        app.launch()
        XCTAssertTrue(app.buttons["chat-back:fx-comms-main"].waitForExistence(timeout: appear), "the Comms chat is open")
        return app
    }

    private var bubbleQuery: NSPredicate { NSPredicate(format: "identifier BEGINSWITH 'bubble:'") }

    /// The lowest bubble fully on screen (the newest message of the open chat).
    private func lastBubble(_ app: XCUIApplication) -> XCUIElement {
        let bubbles = app.descendants(matching: .any).matching(bubbleQuery)
        XCTAssertTrue(bubbles.firstMatch.waitForExistence(timeout: settle), "the transcript shows bubbles")
        let window = app.windows.firstMatch.frame
        let visible = bubbles.allElementsBoundByIndex.filter { $0.isHittable && $0.frame.maxY < window.maxY - 120 && $0.frame.minY > 120 }
        XCTAssertFalse(visible.isEmpty, "a bubble is hittable")
        return visible.last!
    }

    private func chip(_ app: XCUIApplication, _ emoji: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@", "\(emoji) reaction,")).firstMatch
    }

    private func assertGone(_ element: XCUIElement, _ what: String, file: StaticString = #filePath, line: UInt = #line) {
        let gone = NSPredicate(format: "exists == false")
        let wait = XCTNSPredicateExpectation(predicate: gone, object: element)
        XCTAssertEqual(XCTWaiter().wait(for: [wait], timeout: settle), .completed, what, file: file, line: line)
    }

    private func shot(_ app: XCUIApplication, _ name: String) {
        guard let dir = ProcessInfo.processInfo.environment["ARBOS_SHOT_DIR"], !dir.isEmpty else { return }
        let png = XCUIScreen.main.screenshot().pngRepresentation
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
    }

    func testEmojiFromTheKeyboardBecomesOneReaction() {
        let app = launch()
        let bubble = lastBubble(app)
        XCTAssertFalse(chip(app, "🔥").exists, "no 🔥 chip before the pick")
        shot(app, "1-chat")

        bubble.press(forDuration: 0.8)
        let capsule = app.descendants(matching: .any)["message-actions-capsule"]
        XCTAssertTrue(capsule.waitForExistence(timeout: settle), "the reaction capsule opened")
        shot(app, "2-capsule")

        let more = app.buttons["More emoji"]
        XCTAssertTrue(more.waitForExistence(timeout: settle), "the add-emoji cell is in the capsule")
        more.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: settle), "the emoji keyboard came up")
        shot(app, "3-keyboard")

        app.typeText("🔥")

        assertGone(capsule, "the overlay closed on the pick")
        let fire = chip(app, "🔥")
        XCTAssertTrue(fire.waitForExistence(timeout: settle), "a 🔥 chip sits under the bubble")
        XCTAssertEqual(fire.label, "🔥 reaction, 1", "exactly one 🔥 — the pick toggled once")
        shot(app, "4-reaction")
    }
}
