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

    /// A result that is **wholly** on screen and has held still across two reads.
    ///
    /// Both halves are load-bearing. A row that merely overlaps the screen can have its centre
    /// above the top edge, and `tap()` then has to resolve the touch somewhere other than the
    /// point it was given. And the filtered list is still settling for a moment after the last
    /// keystroke, so a frame read even a few hundred milliseconds early describes where the row
    /// *was*. Either one puts the touch on a different row than the test believes it tapped.
    private func settledResult(_ app: XCUIApplication, screen: CGRect) -> (id: String, frame: CGRect)? {
        var previous: [String: CGRect] = [:]
        for _ in 0..<12 {
            let now = bubbles(app, screen: screen)
            let whole = now.filter { screen.contains($0.frame) && $0.frame.height > 0 }
            if let held = whole.first(where: { previous[$0.id] == $0.frame }) {
                return (id: held.id, frame: held.frame)
            }
            previous = Dictionary(now.map { ($0.id, $0.frame) }, uniquingKeysWith: { first, _ in first })
            usleep(400_000)
        }
        return nil
    }

    /// Whether a person's profile pane is covering the chat. Existence is the test, not
    /// hittability: the pane is what sits in front, so its own elements are the frontmost ones.
    private func profilePaneIsUp(_ app: XCUIApplication) -> Bool {
        app.descendants(matching: .any)["profile-message"].firstMatch.exists
            || app.staticTexts["Block user"].exists
    }

    /// What a reader does: type a keyword and tap a result at once, with the keyboard still up.
    /// The bar goes, the filter with it, the reader stays in the chat, and the transcript comes
    /// to rest on the message that was tapped.
    func testTappingAResultKeepsTheReaderInTheChatOnThatMessage() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        let field = app.textFields["chat-search-field"]
        let header = app.descendants(matching: .any)["chat-search-button"].firstMatch

        field.tap()
        field.typeText(Self.keyword)
        sleep(2)
        XCTAssertEqual(app.keyboards.count, 1, "the keyboard is up, as it is for a reader who just typed")
        let results = bubbles(app, screen: screen)
        guard let chosen = settledResult(app, screen: screen) else {
            return XCTFail("no result row is wholly on screen and still (results: \(describe(results)))")
        }
        shot(app, "1-results-keyboard-up")

        app.descendants(matching: .any)[chosen.id].tap()

        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        // Past the reveal's own settle hops, so the rest of this reads where it came to rest.
        sleep(3)
        shot(app, "2-after-the-result-tap")

        let revealed = app.descendants(matching: .any)[chosen.id]
        let after = bubbles(app, screen: screen)
        let where_ = "tapped \(chosen.id) at \(chosen.frame); screen \(screen); "
            + "transcript holding [\(describe(after.filter(\.onScreen)))]"

        // 1. The chat is still the screen. Until this holds, every frame below is a frame from
        //    behind whatever is in front, and the attached screenshot says who.
        XCTAssertFalse(profilePaneIsUp(app),
                       "the tap leaves the reader in the chat, but a profile pane is in front "
                       + "of it — see the 2-after-the-result-tap screenshot — \(where_)")
        XCTAssertTrue(header.isHittable,
                      "the chat header is reachable, so the chat is frontmost — \(where_)")

        // 2. The bar and its filter are gone.
        XCTAssertFalse(field.exists, "a result tap closes the bar — \(where_)")

        // 3. The message that was tapped is built, on screen, and near the middle. The middle
        //    half rules out a landing jammed under the composer or up behind the header without
        //    naming either one's height; the keyword's hits are mid-history, so nothing here is
        //    clamped against an end of the transcript.
        XCTAssertTrue(revealed.exists, "the tapped message is realized in the transcript — \(where_)")
        XCTAssertTrue(screen.contains(revealed.frame),
                      "the tapped message is wholly on screen at \(revealed.frame) — \(where_)")
        let middleHalf = screen.insetBy(dx: 0, dy: screen.height / 4)
        XCTAssertTrue(middleHalf.contains(CGPoint(x: revealed.frame.midX, y: revealed.frame.midY)),
                      "…and centred, not against an edge: middle \(revealed.frame.midY) is outside "
                      + "\(Int(middleHalf.minY))…\(Int(middleHalf.maxY)) — \(where_)")
    }
}
