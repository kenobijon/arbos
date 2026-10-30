import XCTest

/// A tap on a Search-in-chat result must land the full transcript on that message. The
/// fixture group padded with `ARBOS_FIXTURE_LONG_ROWS` is what makes this a real test:
/// the hit sits far above the lazy window, so the reveal has to grow the window before it
/// scrolls. `ChatSearchCloseUITests` covers the short-chat case, where the window already
/// holds every row and nothing has to grow.
final class ChatSearchResultJumpUITests: XCTestCase {
    private let appear: TimeInterval = 150
    private let settle: TimeInterval = 20
    /// Rows the long fixture writes, newest last. 420 rows puts `long-8` about 410 rows
    /// above the tail — far outside the 80-row opening window and the 360-row maximum.
    private static let longRows = 420
    /// `long-8` carries the fixture's "embargo" line (`lines[0]`, every eighth row) and is
    /// the oldest hit a keyword search finds. `RowView` identifies bubbles by message id.
    private static let oldHitId = "bubble:long-8"
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

    /// Scroll the filtered result list up until the oldest hit is on screen. With a keyword
    /// set, a scroll keeps the bar open (`ChatSearchClose.scrollCloses`), so this is safe.
    private func scrollResultsToOldestHit(_ app: XCUIApplication, _ hit: XCUIElement) -> Int {
        var swipes = 0
        while !(hit.exists && hit.isHittable), swipes < 40 {
            app.swipeDown()
            swipes += 1
        }
        return swipes
    }

    /// The topmost result row that is on screen, the one a reader taps without scrolling.
    /// While a keyword filters the list every `bubble:long-…` in the tree is a hit.
    private func topVisibleHit(_ app: XCUIApplication) -> XCUIElement? {
        let hits = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "bubble:long-"))
        for i in 0..<hits.count {
            let e = hits.element(boundBy: i)
            if e.exists, e.isHittable { return e }
        }
        return nil
    }

    /// What a reader actually does: type a keyword and tap a result at once, with the
    /// keyboard still up. Closing the bar drops the keyboard, so the transcript's bottom
    /// inset shrinks by the keyboard's height while the reveal is scrolling.
    func testTappingAResultWithTheKeyboardUpLandsOnThatMessage() {
        let app = openLongLaunchWeekSearch()
        let field = app.textFields["chat-search-field"]
        field.tap()
        field.typeText(Self.keyword)
        XCTAssertTrue(app.descendants(matching: .any)[Self.oldHitId].waitForExistence(timeout: settle),
                      "the filter is on: the oldest hit joined the result list")
        XCTAssertEqual(app.keyboards.count, 1, "the keyboard is up, as it is for a reader who just typed")

        guard let hit = topVisibleHit(app) else {
            return XCTFail("no result row is on screen to tap")
        }
        let id = hit.identifier
        let screen = app.windows.firstMatch.frame
        let before = hit.frame
        shot(app, "1-hit-on-screen-keyboard-up")

        hit.tap()

        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        XCTAssertFalse(field.exists, "a result tap closes the bar")

        let revealed = app.descendants(matching: .any)[id]
        _ = revealed.waitForExistence(timeout: settle)
        sleep(2)
        shot(app, "2-after-the-tap-keyboard-up")

        let after = revealed.exists ? revealed.frame : .null
        XCTAssertTrue(revealed.exists,
                      "\(id) is still in the transcript after the reveal (screen \(screen), was at \(before))")
        XCTAssertTrue(screen.contains(after),
                      "\(id) is wholly on screen after the reveal — it landed at \(after), screen \(screen) "
                      + "(it was at \(before) in the results)")
        XCTAssertTrue(revealed.isHittable, "…and visible (landed at \(after), screen \(screen))")
    }

    func testTappingAnOldResultLandsTheTranscriptOnThatMessage() {
        let app = openLongLaunchWeekSearch()
        let field = app.textFields["chat-search-field"]
        field.tap()
        field.typeText(Self.keyword)

        let hit = app.descendants(matching: .any)[Self.oldHitId]
        let swipes = scrollResultsToOldestHit(app, hit)
        XCTAssertTrue(hit.exists && hit.isHittable,
                      "the oldest hit \(Self.oldHitId) is reachable in the result list (gave up after \(swipes) swipes)")
        let screen = app.windows.firstMatch.frame
        let before = hit.frame
        shot(app, "1-oldest-hit-in-results")

        hit.tap()

        // The bar goes with the tap; the reveal follows it.
        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        XCTAssertFalse(field.exists, "a result tap closes the bar")

        // Give the reveal every chance to settle before reading where it landed.
        _ = hit.waitForExistence(timeout: settle)
        sleep(2)
        shot(app, "2-after-the-result-tap")

        let after = hit.exists ? hit.frame : .null
        XCTAssertTrue(hit.exists,
                      "the tapped message is still in the transcript after the reveal "
                      + "(screen \(screen), it was at \(before) in the results, \(swipes) swipes)")
        XCTAssertTrue(screen.contains(after),
                      "the tapped message is wholly on screen after the reveal — it landed at \(after), "
                      + "screen \(screen) (it was at \(before) in the results)")
        XCTAssertTrue(hit.isHittable,
                      "…and visible, not behind the chrome (landed at \(after), screen \(screen))")
    }
}
