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
/// The first three cases began as one experiment and are now the guard. `SearchResultTap` is a
/// `simultaneousGesture`, by design, so that a link inside a bubble still reaches the system —
/// but the sender's avatar and the name inside the bubble are `Button`s too (`sender-avatar`,
/// `bubble-name`, both calling `toggleProfile`), and they fired alongside the row. Run split,
/// a tap clear of both buttons passed and a tap on the avatar failed, which is what named the
/// cause; the page that opened was an agent's, so nothing about it was person-specific. All
/// three assert the same thing, because staying in the chat is correct wherever inside a
/// result the touch lands (`ChatSearchClose.rowOwnsInnerTap`). The fourth is the other half of
/// that rule: with nothing filtering, the same avatar must still open the page.
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

    private func launchLongLaunchWeek() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = [
            "ARBOS_FIXTURE": "1",
            "ARBOS_OPEN_PEOPLE": "fx-group-launch",
            "ARBOS_FIXTURE_LONG_ROWS": "\(Self.longRows)",
        ]
        app.launch()
        let search = app.descendants(matching: .any)["chat-search-button"].firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: appear), "the chat header's search disc is on screen")
        return app
    }

    private func openLongLaunchWeekSearch() -> XCUIApplication {
        let app = launchLongLaunchWeek()
        let search = app.descendants(matching: .any)["chat-search-button"].firstMatch
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

    /// Whether a profile pane is covering the chat. Existence is the test, not hittability:
    /// the pane is what sits in front, so its own elements are the frontmost ones.
    ///
    /// A diagnosis, not the verdict. `profile-*` comes from a person pane's hero pill
    /// (`PanePillButton`), and an **agent**'s page has none of them — its own pills live in a
    /// section far down Details and overwrite the identifier. So this returning false does
    /// not mean the chat is in front, which is exactly how the measured run behaved: the
    /// avatar case failed on `headerHittable` with this marker still false, on an agent's
    /// page. Whether the chat is frontmost is `headerHittable`'s job; this only names the
    /// thing in front when it can.
    private func profilePaneIsUp(_ app: XCUIApplication) -> Bool {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "profile-"))
            .firstMatch.exists
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

    /// The part of the window a touch actually reaches: below the header and the Search bar,
    /// above the keyboard. Being inside the window is not the same as being reachable, and the
    /// difference is not theoretical — the first row this picked for the in-bubble name case
    /// had the name at y 33, behind the bar, and the tap went to the bar.
    private func reachable(_ app: XCUIApplication, screen: CGRect) -> CGRect {
        var top = screen.minY
        for chrome in [app.descendants(matching: .any)["chat-search-button"].firstMatch,
                       app.textFields["chat-search-field"]] where chrome.exists {
            top = max(top, chrome.frame.maxY)
        }
        var bottom = screen.maxY
        if app.keyboards.count > 0 { bottom = min(bottom, app.keyboards.firstMatch.frame.minY) }
        return CGRect(x: screen.minX, y: top, width: screen.width, height: max(0, bottom - top))
    }

    /// Type the keyword, then pick the result row to tap: wholly on screen, holding still, with
    /// `child` realized inside it, and — the part that matters — with the point `aim` asks for
    /// actually reachable. Each case says where in the row it is aiming, so the pick and the
    /// touch cannot disagree.
    private func settledResult(_ app: XCUIApplication, screen: CGRect, showing child: String,
                               aim: (_ bubble: CGRect, _ child: CGRect) -> CGPoint)
        -> (id: String, frame: CGRect, point: CGPoint)? {
        let field = app.textFields["chat-search-field"]
        field.tap()
        field.typeText(Self.keyword)
        sleep(2)
        let band = reachable(app, screen: screen)
        let results = bubbles(app, screen: screen)
        for candidate in settledResults(app, screen: screen) {
            let inner = inRow(app, candidate.id, child)
            guard inner.exists else { continue }
            let point = aim(candidate.frame, inner.frame)
            guard band.contains(point) else { continue }
            return (id: candidate.id, frame: candidate.frame, point: point)
        }
        XCTFail("no result row is wholly on screen, still, showing \(child), with somewhere "
                + "inside it to tap that is reachable at all (reachable \(band); "
                + "results: \(describe(results)))")
        return nil
    }

    /// Tap, wait for the bar to go, then wait past the reveal's animation so everything after
    /// this reads where the transcript came to rest rather than where it was passing through.
    private func tapAndSettle(_ app: XCUIApplication, at point: CGPoint) {
        tap(app, at: point)
        let field = app.textFields["chat-search-field"]
        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        sleep(3)
    }

    /// What a reader does: type a keyword and tap a result at once, with the keyboard still up.
    /// The touch lands in the bubble's bottom padding — inside the row, clear of the sender's
    /// avatar and of the name, and clear of the text, so no `Button` inside the bubble is under
    /// it. The bar goes, the reader stays in the chat, and the transcript comes to rest on the
    /// message that was tapped. This is the case that passed before the fix: nothing under the
    /// touch but the row, so nothing fired alongside the reveal.
    func testAResultTapClearOfTheAvatarAndNameRevealsTheMessageInTheChat() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        // `Theme.bubbleVPad` is 11, so the bottom few points of the bubble are its own padding:
        // inside the row's hit area and below the last line of text. The name column's midX
        // keeps it inside the bubble horizontally, away from the avatar lane.
        guard let chosen = settledResult(app, screen: screen, showing: "bubble-name",
                                         aim: { bubble, name in
                                             CGPoint(x: name.midX, y: bubble.maxY - 5)
                                         }) else { return }
        XCTAssertEqual(app.keyboards.count, 1, "the keyboard is up, as it is for a reader who just typed")
        let point = chosen.point
        let name = inRow(app, chosen.id, "bubble-name").frame
        let avatar = inRow(app, chosen.id, "sender-avatar")
        XCTAssertFalse(name.contains(point), "the touch is clear of the name button at \(name)")
        if avatar.exists {
            XCTAssertFalse(avatar.frame.contains(point),
                           "the touch is clear of the avatar button at \(avatar.frame)")
        }
        shot(app, "1-results-keyboard-up")

        tapAndSettle(app, at: point)
        shot(app, "2-after-a-tap-clear-of-both-buttons")

        assertLandedInChat(outcome(app, tapped: chosen.id, point: point, screen: screen))
    }

    /// The same tap, on the sender's avatar inside the result row — the case that failed, and
    /// the one the fix is for. Staying in the chat is correct here too: while a keyword filters
    /// the list the row owns the tap, and a reader aiming at a result has not asked to leave
    /// the conversation. What it used to do was reveal the message *and* push the sender's page
    /// over it, because the avatar is a `Button` firing alongside the row's `simultaneousGesture`.
    func testAResultTapOnTheSenderAvatarAlsoStaysInTheChat() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        guard let chosen = settledResult(app, screen: screen, showing: "sender-avatar",
                                         aim: { _, avatar in
                                             CGPoint(x: avatar.midX, y: avatar.midY)
                                         }) else { return }
        let point = chosen.point
        shot(app, "1-results-keyboard-up")

        tapAndSettle(app, at: point)
        shot(app, "2-after-a-tap-on-the-sender-avatar")

        assertLandedInChat(outcome(app, tapped: chosen.id, point: point, screen: screen))
    }

    /// And on the name inside the bubble, the other `Button` on the same profile target. It is
    /// its own case because it is the one the tester reached by accident on a short message,
    /// where the name takes up most of the bubble.
    func testAResultTapOnTheNameInTheBubbleAlsoStaysInTheChat() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        guard let chosen = settledResult(app, screen: screen, showing: "bubble-name",
                                         aim: { _, name in
                                             CGPoint(x: name.midX, y: name.midY)
                                         }) else { return }
        let point = chosen.point
        shot(app, "1-results-keyboard-up")

        tapAndSettle(app, at: point)
        shot(app, "2-after-a-tap-on-the-name-in-the-bubble")

        assertLandedInChat(outcome(app, tapped: chosen.id, point: point, screen: screen))
    }

    /// The suppression is scoped to the filter, and this says so from the outside: with no
    /// keyword, the same avatar in the same chat still opens the sender's page. A reader who is
    /// only reading must not notice this fix at all.
    func testWithNoFilterTheSameAvatarStillOpensTheSendersPage() {
        let app = launchLongLaunchWeek()
        let screen = app.windows.firstMatch.frame
        let band = reachable(app, screen: screen)
        let resting = bubbles(app, screen: screen)
        var picked: (id: String, point: CGPoint)?
        for candidate in settledResults(app, screen: screen) where picked == nil {
            let avatar = inRow(app, candidate.id, "sender-avatar")
            guard avatar.exists else { continue }
            let point = CGPoint(x: avatar.frame.midX, y: avatar.frame.midY)
            if band.contains(point) { picked = (id: candidate.id, point: point) }
        }
        guard let chosen = picked else {
            return XCTFail("no bubble is wholly on screen, still, with a reachable sender avatar "
                           + "(reachable \(band); rows: \(describe(resting)))")
        }
        shot(app, "1-chat-at-rest-nothing-filtering")

        tap(app, at: chosen.point)
        sleep(3)
        shot(app, "2-after-a-tap-on-the-avatar-with-nothing-filtering")

        let header = app.descendants(matching: .any)["chat-search-button"].firstMatch
        XCTAssertFalse(header.isHittable,
                       "with nothing filtering, a tap on the avatar opens the sender's page over "
                       + "the chat as it always has — profile pane marker seen: "
                       + "\(profilePaneIsUp(app)); tapped \(chosen.id) avatar at \(avatar)")
    }
}
