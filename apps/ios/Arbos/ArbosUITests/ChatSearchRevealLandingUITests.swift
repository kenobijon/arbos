import XCTest

/// Why a tapped search result sometimes rests short of the message it was given (#2404).
///
/// This is the landing symptom, and it is a different bug from the profile page a touch over
/// the avatar used to push over the chat. It is stated separately here because it survives
/// the frontmost check: the chat is in front, the bar has gone, nothing covers the message —
/// the transcript has simply come to rest somewhere else.
///
/// **The two cases are one experiment and differ in one character.** Both run the identical
/// procedure — same keyword, same nudges to bring the row into reach, same tap point in the
/// bubble's bottom padding clear of every button — and then tap a different result: `long-408`
/// rests about thirty rows short of itself, `long-412` lands. Four rows apart in the same
/// list, so whatever separates them is not the distance from the tail.
///
/// Order matters and `continueAfterFailure` is off so it holds: a coordinate read says nothing
/// until it is established what is frontmost, because XCUITest reports a parent view's
/// elements, with plausible frames, from behind a pushed navigation destination.
final class ChatSearchRevealLandingUITests: XCTestCase {
    private let appear: TimeInterval = 150
    private let settle: TimeInterval = 20
    private static let longRows = 420
    private static let keyword = "embargo"

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func openSearch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = [
            "ARBOS_FIXTURE": "1",
            "ARBOS_OPEN_PEOPLE": "fx-group-launch",
            "ARBOS_FIXTURE_LONG_ROWS": "\(Self.longRows)",
            // The app prints what moves the transcript; the run's stdout is the evidence.
            "ARBOS_TRACE_REVEAL": "1",
        ]
        app.launch()
        let search = app.descendants(matching: .any)["chat-search-button"].firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: appear), "the chat header's search disc is on screen")
        let field = app.textFields["chat-search-field"]
        for _ in 0..<4 where !field.exists {
            sleep(1)
            search.tap()
            _ = field.waitForExistence(timeout: 8)
        }
        XCTAssertTrue(field.exists, "the Search-in-chat bar opened")
        field.tap()
        field.typeText(Self.keyword)
        sleep(2)
        return app
    }

    private func shot(_ app: XCUIApplication, _ name: String) {
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
    }

    private func row(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any)["bubble:\(id)"].firstMatch
    }

    private func inRow(_ app: XCUIApplication, _ id: String, _ child: String) -> XCUIElement {
        row(app, id).descendants(matching: .any)[child].firstMatch
    }

    /// Every realized bubble, as the tree lists them.
    private func bubbles(_ app: XCUIApplication) -> [(id: String, frame: CGRect)] {
        let all = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "bubble:"))
        var out: [(id: String, frame: CGRect)] = []
        for i in 0..<min(all.count, 60) {
            let e = all.element(boundBy: i)
            guard e.exists else { continue }
            out.append((id: e.identifier, frame: e.frame))
        }
        return out
    }

    private func describe(_ rows: [(id: String, frame: CGRect)]) -> String {
        rows.map { "\($0.id)@y\(Int($0.frame.minY))…\(Int($0.frame.maxY))" }.joined(separator: ", ")
    }

    /// The part of the window a touch actually reaches: below the header and the Search bar,
    /// above the keyboard. Being inside the window is not the same as being reachable.
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

    /// Drag the result list, `older` to bring earlier hits down into view and `!older` to
    /// pull later ones up out from behind the keyboard. Both target rows are the third and
    /// second from the end of the hit list, so a one-way nudge would walk away from them.
    /// With a keyword typed a scroll keeps the bar open (`ChatSearchClose.scrollCloses`), so
    /// this changes which results are reachable and nothing else about the state under test.
    private func nudge(_ app: XCUIApplication, band: CGRect, older: Bool) {
        let origin = app.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
        let near = origin.withOffset(CGVector(dx: band.midX, dy: band.minY + 30))
        let far = origin.withOffset(CGVector(dx: band.midX, dy: band.minY + 230))
        if older {
            near.press(forDuration: 0.1, thenDragTo: far)
        } else {
            far.press(forDuration: 0.1, thenDragTo: near)
        }
        sleep(2)
    }

    private func tap(_ app: XCUIApplication, at point: CGPoint) {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
            .withOffset(CGVector(dx: point.x, dy: point.y))
            .tap()
    }

    /// Bring `id` into reach and hand back the point to tap: the bubble's bottom padding.
    /// `Theme.bubbleVPad` is 11, so the bottom few points are the bubble's own padding —
    /// inside the row's hit area, below the last line of text, and clear of the sender's
    /// avatar and of the name, neither of which must take this touch.
    private func aim(_ app: XCUIApplication, screen: CGRect, at id: String) -> CGPoint? {
        for _ in 0..<8 {
            let band = reachable(app, screen: screen)
            let bubble = row(app, id)
            // Which way to drag: toward the row if it is realized but out of the band, and
            // toward older hits when it is not in the tree at all.
            var older = true
            if bubble.exists, bubble.frame.height > 0 {
                let frame = bubble.frame
                let before = frame
                usleep(400_000)
                guard row(app, id).frame == before else { continue }
                let name = inRow(app, id, "bubble-name")
                let x = name.exists ? name.frame.midX : frame.midX
                let point = CGPoint(x: x, y: frame.maxY - 5)
                if band.contains(point), band.contains(CGPoint(x: x, y: frame.minY + 5)) {
                    if name.exists {
                        XCTAssertFalse(name.frame.contains(point),
                                       "the touch is clear of the name button at \(name.frame)")
                    }
                    let avatar = inRow(app, id, "sender-avatar")
                    if avatar.exists {
                        XCTAssertFalse(avatar.frame.contains(point),
                                       "the touch is clear of the avatar button at \(avatar.frame)")
                    }
                    return point
                }
                older = frame.minY < band.minY
            }
            nudge(app, band: band, older: older)
        }
        return nil
    }

    /// Tap `id` and say where the transcript came to rest. Identical for both cases.
    private func landing(_ id: String) {
        let app = openSearch()
        let screen = app.windows.firstMatch.frame
        guard let point = aim(app, screen: screen, at: id) else {
            return XCTFail("could not bring \(id) into reach after eight nudges "
                           + "(results: \(describe(bubbles(app))))")
        }
        shot(app, "1-\(id)-aimed")
        // The test runner's stdout lands in the xcodebuild log, which arrives whatever the
        // result bundle does. It is the only record of the pre-tap state on a green run, and
        // the two cases are only comparable if that state matches.
        print("[rig] \(id) aimed at \(point); results before the tap: \(describe(bubbles(app)))")

        tap(app, at: point)
        let field = app.textFields["chat-search-field"]
        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        sleep(3)
        shot(app, "2-\(id)-after-the-tap")

        // Frontmost first: a frame means nothing until the chat is what the reader sees.
        let header = app.descendants(matching: .any)["chat-search-button"].firstMatch
        let resting = bubbles(app)
        let detail = "tapped \(id) at \(point); screen \(screen); resting [\(describe(resting))]"
        print("[rig] \(detail)")
        XCTAssertTrue(header.isHittable, "the chat is frontmost — \(detail)")
        XCTAssertFalse(field.exists, "a result tap closes the bar — \(detail)")

        let revealed = row(app, id)
        XCTAssertTrue(revealed.exists, "the tapped message is realized in the transcript — \(detail)")
        XCTAssertTrue(screen.contains(revealed.frame),
                      "the tapped message is wholly on screen at \(revealed.frame) — \(detail)")
        let middleHalf = screen.insetBy(dx: 0, dy: screen.height / 4)
        XCTAssertTrue(middleHalf.contains(CGPoint(x: revealed.frame.midX, y: revealed.frame.midY)),
                      "…and centred, not against an edge — \(detail)")
    }

    /// The result that rests short. Measured twice before this test existed, once from behind
    /// a profile pane and once with the chat frontmost, and both times the transcript came to
    /// rest on `long-375`…`long-383` with `long-408` never realized.
    func testTappingLong408RestsOnTheMessage() {
        landing("long-408")
    }

    /// The control: four rows further down the same list, same procedure, lands every time.
    func testTappingLong412RestsOnTheMessage() {
        landing("long-412")
    }
}
