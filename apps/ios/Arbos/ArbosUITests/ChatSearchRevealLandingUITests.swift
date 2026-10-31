import XCTest

/// Why a tapped search result sometimes rests short of the message it was given (#2404).
///
/// This is the landing symptom, and it is a different bug from the profile page a touch over
/// the avatar used to push over the chat. It is stated separately here because it survives
/// the frontmost check: the chat is in front, the bar has gone, nothing covers the message —
/// the transcript has simply come to rest somewhere else.
///
/// **The four cases are one experiment over two conditions.** Every case runs the identical
/// procedure — same keyword, same nudges to bring the row into reach, same tap point in the
/// bubble's bottom padding clear of every button — and varies two things only: which result
/// is tapped (`long-408`, which rests short, against `long-412`, which lands, four rows apart
/// in the same list, so whatever separates them is not the distance from the tail) and
/// whether the near-top window grow is allowed to run inside the reveal.
///
/// Reading them as a block: if the grow is the mechanism, `long-408` fails with it allowed
/// and lands with it held, and `long-412` lands either way. Any other pattern says the grow
/// is not it, which is as much as this rig is for.
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

    private func openSearch(allowGrow: Bool) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = [
            "ARBOS_FIXTURE": "1",
            "ARBOS_OPEN_PEOPLE": "fx-group-launch",
            "ARBOS_FIXTURE_LONG_ROWS": "\(Self.longRows)",
            // The app prints what moves the transcript; the run's stdout is the evidence.
            "ARBOS_TRACE_REVEAL": "1",
        ]
        if allowGrow { app.launchEnvironment["ARBOS_REVEAL_ALLOW_GROW"] = "1" }
        app.launch()
        let search = app.descendants(matching: .any)["chat-search-button"].firstMatch
        // Poll rather than one long wait, and give up the moment the app is gone. A trap in
        // the app under test looks identical to a slow Mac for the whole timeout, and four
        // cases each sat out 150 s of a contended heavy slot before saying so once.
        let deadline = Date().addingTimeInterval(appear)
        while !search.exists, Date() < deadline {
            XCTAssertEqual(app.state, .runningForeground,
                           "the app is still running (state \(app.state.rawValue)) — it died "
                           + "before the chat opened, so nothing below this is about the chat")
            usleep(500_000)
        }
        XCTAssertTrue(search.exists, "the chat header's search disc is on screen")
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

    /// Get as far as a touch aimed at `id`, or fail. Everything here is harness, not finding:
    /// the app launches, the bar opens, the row comes into reach. Kept apart from the landing
    /// so the symptom arm's expected-failure wrapper cannot swallow a broken rig — it did
    /// exactly that once, reporting a run whose app died at launch as the symptom reproduced.
    private func aimed(_ id: String, allowGrow: Bool) -> (app: XCUIApplication, screen: CGRect, point: CGPoint)? {
        let app = openSearch(allowGrow: allowGrow)
        let screen = app.windows.firstMatch.frame
        guard let point = aim(app, screen: screen, at: id) else {
            XCTFail("could not bring \(id) into reach after eight nudges "
                    + "(results: \(describe(bubbles(app))))")
            return nil
        }
        return (app, screen, point)
    }

    /// Tap the aimed point and say where the transcript came to rest. Identical for every
    /// case but the row id and whether the near-top grow may run inside the reveal.
    private func landing(_ id: String, allowGrow: Bool,
                         _ aimed: (app: XCUIApplication, screen: CGRect, point: CGPoint)) {
        let (app, screen, point) = aimed
        let arm = "\(id)/\(allowGrow ? "grow-allowed" : "grow-held")"
        shot(app, "1-\(arm)-aimed")
        // The test runner's stdout lands in the result bundle's Staging tree, which survives
        // the import that never finishes. It is the only record of the pre-tap state on a
        // green run, and the four cases are only comparable if that state matches.
        print("[rig] \(arm) aimed at \(point); results before the tap: \(describe(bubbles(app)))")

        tap(app, at: point)
        let field = app.textFields["chat-search-field"]
        let deadline = Date().addingTimeInterval(settle)
        while field.exists, Date() < deadline { usleep(150_000) }
        sleep(3)
        shot(app, "2-\(arm)-after-the-tap")

        // Frontmost first: a frame means nothing until the chat is what the reader sees.
        let header = app.descendants(matching: .any)["chat-search-button"].firstMatch
        let resting = bubbles(app)
        let detail = "tapped \(arm) at \(point); screen \(screen); resting [\(describe(resting))]"
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

    /// The whole case for an arm that is expected to land.
    private func run(_ id: String, allowGrow: Bool) {
        guard let a = aimed(id, allowGrow: allowGrow) else { return }
        landing(id, allowGrow: allowGrow, a)
    }

    /// The row that rests short, with the near-top grow left as it is on `main`. Measured
    /// twice before this test existed, once from behind a profile pane and once with the chat
    /// frontmost, and both times the transcript came to rest on `long-375`…`long-383` with
    /// `long-408` never realized.
    ///
    /// This case is the reproduction, so it is expected to fail, and the expectation is
    /// recorded rather than asserted: the symptom is the thing under investigation and a
    /// lane that has not yet proved it cannot also claim how reliably it fires. The log says
    /// which way it went either way, and `strict: false` keeps the run's verdict about the
    /// other three. It goes away with the rig flag once the mechanism is settled.
    ///
    /// Only the landing is inside the expectation. Getting the app up and the touch aimed is
    /// outside it, so a rig that breaks goes red here instead of passing as a reproduction.
    func testLong408WithTheGrowAllowedIsTheSymptom() {
        guard let a = aimed("long-408", allowGrow: true) else { return }
        continueAfterFailure = true
        XCTExpectFailure("the near-top grow inside the reveal is the bug under test (#2404)",
                         strict: false) {
            landing("long-408", allowGrow: true, a)
        }
    }

    /// The same row with the window held still for the length of the reveal. The one changed
    /// condition between this and the case above, so a difference in the landing is the
    /// grow's doing and nothing else's.
    func testLong408WithTheGrowHeldRestsOnTheMessage() {
        run("long-408", allowGrow: false)
    }

    /// The control row, four further down the same list, with the grow allowed: it lands.
    /// Whatever separates the two rows, it is not the distance from the tail.
    func testLong412WithTheGrowAllowedRestsOnTheMessage() {
        run("long-412", allowGrow: true)
    }

    /// The control row with the hold on, so the hold is shown not to break a landing that
    /// already worked.
    func testLong412WithTheGrowHeldRestsOnTheMessage() {
        run("long-412", allowGrow: false)
    }
}
