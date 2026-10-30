import XCTest

/// A tap on a Search-in-chat result lands the full transcript on that message (#2404).
/// The fixture group padded with `ARBOS_FIXTURE_LONG_ROWS` is what makes this a real test:
/// the transcript is far longer than the lazy window, so most of the rows restored when the
/// filter goes have estimated heights at the moment the reveal scrolls. On the short fixture
/// chat `ChatSearchCloseUITests` covers, the content is shorter than the viewport and the
/// view cannot move at all.
///
/// Measured on iPhone 17 / iOS 27.0 before the fix: a tap on `long-408` left the transcript
/// showing `long-375`…`long-383` — about 30 messages short — with `long-408` not realized.
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

    /// What a reader does: type a keyword and tap a result at once, with the keyboard still
    /// up. The bar goes, the filter with it, and the transcript must come to rest on the
    /// message that was tapped — not on the rows some screens above it.
    func testTappingAResultLandsTheTranscriptOnThatMessage() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        let field = app.textFields["chat-search-field"]

        field.tap()
        field.typeText(Self.keyword)
        sleep(2)
        let results = bubbles(app, screen: screen)
        XCTAssertEqual(app.keyboards.count, 1, "the keyboard is up, as it is for a reader who just typed")
        guard let chosen = results.first(where: { $0.onScreen }) else {
            return XCTFail("no result row is on screen to tap (results: \(describe(results)))")
        }
        shot(app, "1-results-keyboard-up")

        app.descendants(matching: .any)[chosen.id].tap()

        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        XCTAssertFalse(field.exists, "a result tap closes the bar")

        let revealed = app.descendants(matching: .any)[chosen.id]
        _ = revealed.waitForExistence(timeout: settle)
        // Past the reveal's own settle hops, so this reads where it came to rest.
        sleep(3)
        shot(app, "2-after-the-result-tap")

        let after = bubbles(app, screen: screen)
        let where_ = "tapped \(chosen.id) at \(chosen.frame); screen \(screen); "
            + "landed with [\(describe(after.filter(\.onScreen)))] on screen"
        XCTAssertTrue(revealed.exists, "the tapped message is realized in the transcript — \(where_)")
        XCTAssertTrue(screen.contains(revealed.frame),
                      "the tapped message is wholly on screen at \(revealed.frame) — \(where_)")
        XCTAssertTrue(revealed.isHittable, "…and visible, not behind the chrome — \(where_)")
    }
}
