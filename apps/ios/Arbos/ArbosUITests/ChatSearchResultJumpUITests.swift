import XCTest

/// A tap on a Search-in-chat result must land the full transcript on that message. The
/// fixture group padded with `ARBOS_FIXTURE_LONG_ROWS` is what makes this a real test:
/// the transcript is longer than the lazy window, so the reveal has to move the view.
/// `ChatSearchCloseUITests` covers the short-chat case, where the content is shorter than
/// the viewport and nothing can move.
final class ChatSearchResultJumpUITests: XCTestCase {
    private let appear: TimeInterval = 150
    private let settle: TimeInterval = 20
    /// Rows the long fixture writes, newest last.
    private static let longRows = 420
    /// The fixture's "embargo" line is `lines[0]`, every eighth synthetic row.
    private static let keyword = "embargo"
    /// The fixture's own newest row (`Fixture.swift`, g9) — on screen when the transcript
    /// sits at the tail instead of on the tapped result.
    private static let newestRowText = "1,284 signups"

    override func setUpWithError() throws {
        continueAfterFailure = true
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

    /// Every realized transcript bubble, newest-to-oldest as the tree lists them, with the
    /// part of each frame that is inside `screen`.
    private func bubbles(_ app: XCUIApplication, screen: CGRect) -> [(id: String, frame: CGRect, onScreen: Bool)] {
        let all = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "bubble:"))
        var out: [(String, CGRect, Bool)] = []
        for i in 0..<min(all.count, 60) {
            let e = all.element(boundBy: i)
            guard e.exists else { continue }
            let f = e.frame
            out.append((e.identifier, f, screen.intersects(f) && f.height > 0))
        }
        return out.map { (id: $0.0, frame: $0.1, onScreen: $0.2) }
    }

    private func describe(_ rows: [(id: String, frame: CGRect, onScreen: Bool)]) -> String {
        rows.map { "\($0.id)@y\(Int($0.frame.minY))…\(Int($0.frame.maxY))\($0.onScreen ? "" : " (off)")" }
            .joined(separator: ", ")
    }

    /// Observation pass. Types a keyword and taps a result the way a reader does — at once,
    /// with the keyboard still up — then records where the transcript ended up: the tapped
    /// bubble's frame, which bubbles are on screen, whether the tail is showing instead.
    /// Deliberately reports everything in one failure so the run's log carries the evidence.
    func testObserveWhereAResultTapLandsTheTranscript() {
        let app = openLongLaunchWeekSearch()
        let screen = app.windows.firstMatch.frame
        let field = app.textFields["chat-search-field"]

        let beforeTyping = bubbles(app, screen: screen)
        field.tap()
        field.typeText(Self.keyword)
        sleep(2)
        let filtered = bubbles(app, screen: screen)
        let keyboardsWhileTyping = app.keyboards.count
        shot(app, "1-results-keyboard-up")

        // The topmost result row on screen: what a reader taps without scrolling.
        let chosen = filtered.first { $0.onScreen }
        guard let chosen else {
            return XCTFail("OBSERVE: no result row on screen. screen=\(screen) "
                           + "beforeTyping=[\(describe(beforeTyping))] filtered=[\(describe(filtered))]")
        }
        let target = app.descendants(matching: .any)[chosen.id]
        target.tap()

        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        let barClosed = !field.exists
        sleep(3)
        shot(app, "2-after-the-result-tap")

        let after = bubbles(app, screen: screen)
        let landed = after.first { $0.id == chosen.id }
        let newest = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", Self.newestRowText)).firstMatch
        let tailShowing = newest.exists && screen.intersects(newest.frame)
        let onScreenAfter = after.filter(\.onScreen)

        XCTFail("""
            OBSERVE result-tap landing
              screen: \(screen)
              bubbles before typing (\(beforeTyping.count)): \(describe(beforeTyping))
              keyboards while typing: \(keyboardsWhileTyping)
              bubbles with the filter on (\(filtered.count)): \(describe(filtered))
              tapped: \(chosen.id) at \(chosen.frame)
              bar closed: \(barClosed); keyboards after: \(app.keyboards.count)
              tapped bubble after: \(landed.map { "\($0.id) at \($0.frame) onScreen=\($0.onScreen)" } ?? "NOT IN TREE")
              tapped bubble hittable after: \(target.exists ? "\(target.isHittable)" : "n/a")
              on screen after (\(onScreenAfter.count)): \(describe(onScreenAfter))
              all bubbles after (\(after.count)): \(describe(after))
              tail row showing: \(tailShowing) \(newest.exists ? "at \(newest.frame)" : "(absent)")
            """)
    }
}
