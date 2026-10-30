import XCTest

/// A tap on a Search-in-chat result must leave the reader in the chat, looking at the message
/// they tapped (#2404). The fixture group padded with `ARBOS_FIXTURE_LONG_ROWS` is what makes
/// this a real test: the transcript is far longer than the lazy window, so most of the rows
/// restored when the filter goes have estimated heights at the moment the reveal scrolls. On
/// the short fixture chat `ChatSearchCloseUITests` covers, the content is shorter than the
/// viewport and the view cannot move at all.
///
/// **Order matters here, and `continueAfterFailure` is off so it holds.** The first version of
/// this test asserted the landing geometry alone and passed — while the app had pushed a
/// member's profile page over the chat. XCUITest reports a parent view's elements, with
/// plausible frames, from behind a pushed navigation destination, so a coordinate read says
/// nothing about what anybody can see. What is frontmost has to be established first; only
/// then does a frame mean anything.
///
/// The two cases below are one experiment. `SearchResultTap` is a `simultaneousGesture`, by
/// design, so that a link or a mention chip inside a bubble still wins — but the sender's
/// avatar and the name inside the bubble are `Button`s too (`sender-avatar`, `bubble-name`,
/// both calling `toggleProfile`). If that is why a profile page appears, then a result tap
/// clear of both buttons must leave the reader in the chat, and a result tap on the avatar
/// must not. Both cases assert the same thing, because staying in the chat is correct for
/// both; which one fails says where the touch is going.
final class ChatSearchResultJumpUITests: XCTestCase {
    private let appear: TimeInterval = 150
    private let settle: TimeInterval = 20
    /// Rows the long fixture writes, newest last.
    private static let longRows = 420
    /// The fixture's "embargo" line is `lines[0]`, every eighth synthetic row.
    private static let keyword = "embargo"

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func openLongLaunchWeekSearch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = [
            "ARBOS_FIXTURE": "1",
            "ARBOS_OPEN_PEOPLE": "fx-group-launch",
            "ARBOS_FIXTURE_LONG_ROWS": "\(Self.longRows)",
        ]
        app.launch()
        let search = app.descendants(matching: .any)["chat-search-button"].firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: appear), "the chat header's search disc is on screen")
        // Let the push settle (a tap during the slide lands on the home list on a loaded Mac).
        let field = app.textFields["chat-search-field"]
        for _ in 0..<4 where !field.exists {
            sleep(1)
            search.tap()
            _ = field.waitForExistence(timeout: 8)
        }
        XCTAssertTrue(field.exists, "the Search-in-chat bar opened")
        return app
    }

    private func shot(_ app: XCUIApplication, _ name: String) {
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
    }

    /// Every realized transcript bubble, as the tree lists them. While a keyword filters the
    /// list each one is a hit; after the close they are the unfiltered rows around the landing.
    private func bubbles(_ app: XCUIApplication, screen: CGRect) -> [(id: String, frame: CGRect, onScreen: Bool)] {
        let all = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "bubble:"))
        var out: [(id: String, frame: CGRect, onScreen: Bool)] = []
        for i in 0..<min(all.count, 60) {
            let e = all.element(boundBy: i)
            guard e.exists else { continue }
            let f = e.frame
            out.append((id: e.identifier, frame: f, onScreen: screen.intersects(f) && f.height > 0))
        }
        return out
    }

    private func describe(_ rows: [(id: String, frame: CGRect, onScreen: Bool)]) -> String {
        rows.map { "\($0.id)@y\(Int($0.frame.minY))…\(Int($0.frame.maxY))\($0.onScreen ? "" : " (off)")" }
            .joined(separator: ", ")
    }

    /// Results that are **wholly** on screen and have held the same frame across two reads.
    ///
    /// Both halves are load-bearing. A row that merely overlaps the screen can have its centre
    /// above the top edge, and `tap()` then has to resolve the touch somewhere other than the
    /// point it was given. And the filtered list is still settling for a moment after the last
    /// keystroke, so a frame read even a few hundred milliseconds early describes where the row
    /// *was*. Either one puts the touch on a different row than the test believes it tapped.
    private func settledResults(_ app: XCUIApplication, screen: CGRect) -> [(id: String, frame: CGRect)] {
        var previous: [String: CGRect] = [:]
        for _ in 0..<12 {
            let now = bubbles(app, screen: screen)
            let held = now
                .filter { screen.contains($0.frame) && $0.frame.height > 0 && previous[$0.id] == $0.frame }
                .map { (id: $0.id, frame: $0.frame) }
            if !held.isEmpty { return held }
            previous = Dictionary(now.map { ($0.id, $0.frame) }, uniquingKeysWith: { first, _ in first })
            usleep(400_000)
        }
        return []
    }

    /// Whether a person's profile pane is covering the chat. Existence is the test, not
    /// hittability: the pane is what sits in front, so its own elements are the frontmost ones.
    private func profilePaneIsUp(_ app: XCUIApplication) -> Bool {
        app.descendants(matching: .any)["profile-message"].firstMatch.exists
            || app.staticTexts["Block user"].exists
    }

    private func row(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any)[id]
    }

    private func inRow(_ app: XCUIApplication, _ id: String, _ child: String) -> XCUIElement {
        row(app, id).descendants(matching: .any)[child].firstMatch
    }

    private func tap(_ app: XCUIApplication, at point: CGPoint) {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
            .withOffset(CGVector(dx: point.x, dy: point.y))
            .tap()
    }

    /// Everything the assertions need, read once so they all describe the same moment, with
    /// `detail` carrying the geometry into every failure message.
    private struct TapOutcome {
        let profileUp: Bool
        let headerHittable: Bool
        let barOpen: Bool
        let revealedExists: Bool
        let revealedFrame: CGRect
        let screen: CGRect
        let detail: String
    }

    private func outcome(_ app: XCUIApplication, tapped: String, point: CGPoint,
                         screen: CGRect) -> TapOutcome {
        let revealed = row(app, tapped)
        let exists = revealed.exists
        let after = bubbles(app, screen: screen).filter { $0.onScreen }
        return TapOutcome(
            profileUp: profilePaneIsUp(app),
            headerHittable: app.descendants(matching: .any)["chat-search-button"].firstMatch.isHittable,
            barOpen: app.textFields["chat-search-field"].exists,
            revealedExists: exists,
            revealedFrame: exists ? revealed.frame : .null,
            screen: screen,
            detail: "tapped \(tapped) at \(point); screen \(screen); "
                + "message realized: \(exists)\(exists ? " at \(revealed.frame)" : ""); "
                + "transcript holding [\(describe(after))]")
    }

    /// Everything a result tap owes the reader, once it is established that the reader is still
    /// in the chat to be owed it.
    private func assertLandedInChat(_ o: TapOutcome) {
        XCTAssertFalse(o.profileUp,
                       "the tap leaves the reader in the chat, but a profile pane is in front of "
                       + "it — see the screenshot — \(o.detail)")
        XCTAssertTrue(o.headerHittable,
                      "the chat header is reachable, so the chat is frontmost — \(o.detail)")
        XCTAssertFalse(o.barOpen, "a result tap closes the bar — \(o.detail)")
        XCTAssertTrue(o.revealedExists,
                      "the tapped message is realized in the transcript — \(o.detail)")
        XCTAssertTrue(o.screen.contains(o.revealedFrame),
                      "the tapped message is wholly on screen at \(o.revealedFrame) — \(o.detail)")
        // The middle half rules out a landing jammed under the composer or up behind the header
        // without naming either one's height. The keyword's hits are mid-history, so nothing
        // here is clamped against an end of the transcript.
        let middleHalf = o.screen.insetBy(dx: 0, dy: o.screen.height / 4)
        XCTAssertTrue(middleHalf.contains(CGPoint(x: o.revealedFrame.midX, y: o.revealedFrame.midY)),
                      "…and centred, not against an edge: middle \(o.revealedFrame.midY) is "
                      + "outside \(Int(middleHalf.minY))…\(Int(middleHalf.maxY)) — \(o.detail)")
    }

    /// What a reader does: type a keyword and tap a result at once, with the keyboard still up.
    /// The touch lands in the bubble's bottom padding — inside the row, clear of the sender's
    /// avatar and of the name, and clear of the text, so no `Button` inside the bubble is under
    /// it. The bar goes, the reader stays in the chat, and the transcript comes to rest on the
    /// message that was tapped.
    func testAResultTapClearOfTheAvatarAndNameRevealsTheMessageInTheChat() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        let field = app.textFields["chat-search-field"]

        field.tap()
        field.typeText(Self.keyword)
        sleep(2)
        XCTAssertEqual(app.keyboards.count, 1, "the keyboard is up, as it is for a reader who just typed")
        let results = bubbles(app, screen: screen)
        let settled = settledResults(app, screen: screen)
        guard let chosen = settled.first(where: { inRow(app, $0.id, "bubble-name").exists }) else {
            return XCTFail("no result row is wholly on screen, still, and showing a sender name "
                           + "(results: \(describe(results)))")
        }
        let name = inRow(app, chosen.id, "bubble-name").frame
        let avatar = inRow(app, chosen.id, "sender-avatar")
        // `Theme.bubbleVPad` is 11, so the bottom few points of the bubble are its own padding:
        // inside the row's hit area and below the last line of text. The name column's midX
        // keeps it inside the bubble horizontally, away from the avatar lane.
        let point = CGPoint(x: name.midX, y: chosen.frame.maxY - 5)
        XCTAssertFalse(name.contains(point), "the touch is clear of the name button at \(name)")
        if avatar.exists {
            XCTAssertFalse(avatar.frame.contains(point),
                           "the touch is clear of the avatar button at \(avatar.frame)")
        }
        shot(app, "1-results-keyboard-up")

        tap(app, at: point)
        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        // Past the reveal's own settle hops, so the rest of this reads where it came to rest.
        sleep(3)
        shot(app, "2-after-a-tap-clear-of-both-buttons")

        assertLandedInChat(outcome(app, tapped: chosen.id, point: point, screen: screen))
    }

    /// The same tap, on the sender's avatar inside the result row. Staying in the chat is
    /// correct here too: the row owns this gesture while a keyword filters the list, and a
    /// reader aiming at a result has not asked to leave the conversation. If this is the case
    /// that fails while the one above passes, the profile page comes from the `Button`s inside
    /// the bubble firing alongside the row's `simultaneousGesture`.
    func testAResultTapOnTheSenderAvatarAlsoStaysInTheChat() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        let field = app.textFields["chat-search-field"]

        field.tap()
        field.typeText(Self.keyword)
        sleep(2)
        let results = bubbles(app, screen: screen)
        let settled = settledResults(app, screen: screen)
        guard let chosen = settled.first(where: { inRow(app, $0.id, "sender-avatar").exists }) else {
            return XCTFail("no result row is wholly on screen, still, and showing a sender avatar "
                           + "(results: \(describe(results)))")
        }
        let avatar = inRow(app, chosen.id, "sender-avatar").frame
        let point = CGPoint(x: avatar.midX, y: avatar.midY)
        shot(app, "1-results-keyboard-up")

        tap(app, at: point)
        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        sleep(3)
        shot(app, "2-after-a-tap-on-the-sender-avatar")

        assertLandedInChat(outcome(app, tapped: chosen.id, point: point, screen: screen))
    }
}
