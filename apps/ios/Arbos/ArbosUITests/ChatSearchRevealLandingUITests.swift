import XCTest

/// Why a tapped search result sometimes rests short of the message it was given (#2404).
///
/// This is the landing symptom, and it is a different bug from the profile page a touch over
/// the avatar used to push over the chat. It is stated separately here because it survives
/// the frontmost check: the chat is in front, the bar has gone, nothing covers the message —
/// the transcript has simply come to rest somewhere else.
///
/// **The cases are one experiment and vary how far back through the results the reader
/// scrolled before tapping.** Everything else is identical: same keyword, same tap point in
/// the bubble's bottom padding clear of every button, same settle.
///
/// That variable replaced an earlier one. The first run tapped two named rows four apart in
/// the list and both landed, with byte-identical offset trajectories from the same starting
/// offset of 8676 — because both were reachable without scrolling the list at all. A reveal
/// animates from wherever the scroll view is standing, so a run that never moves it is one
/// observation repeated, whatever row it taps.
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

    /// One drag back through the results, toward older hits — the reader scrolling the list.
    /// The press is slow and short so it drags rather than flicks: a flick's deceleration
    /// would leave the list somewhere this cannot name. With a keyword typed a scroll keeps
    /// the bar open (`ChatSearchClose.scrollCloses`), so this moves where the list stands and
    /// changes nothing else about the state under test.
    private func nudge(_ app: XCUIApplication, band: CGRect) {
        let origin = app.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
        origin.withOffset(CGVector(dx: band.midX, dy: band.minY + 30))
            .press(forDuration: 0.1,
                   thenDragTo: origin.withOffset(CGVector(dx: band.midX, dy: band.minY + 230)))
        usleep(900_000)
    }

    private func tap(_ app: XCUIApplication, at point: CGPoint) {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
            .withOffset(CGVector(dx: point.x, dy: point.y))
            .tap()
    }

    /// A still hit whose bottom padding is reachable, and the point to tap in it.
    ///
    /// Only the touch point has to be in the band. A tall bubble whose top is behind the
    /// header is still perfectly tappable near its bottom, and requiring the whole row to be
    /// reachable threw away `long-412` — a 292-point markdown bubble — eight nudges running.
    ///
    /// `Theme.bubbleVPad` is 11, so the bottom few points are the bubble's own padding:
    /// inside the row's hit area, below the last line of text, and clear of the sender's
    /// avatar and of the name, neither of which must take this touch. This branch is off
    /// `main`, where nothing suppresses those buttons, so the clearance is asserted.
    private func aimable(_ app: XCUIApplication, screen: CGRect) -> (id: String, point: CGPoint)? {
        let band = reachable(app, screen: screen)
        for (id, frame) in bubbles(app) {
            let point = CGPoint(x: frame.midX, y: frame.maxY - 5)
            guard band.contains(point), frame.height > 24 else { continue }
            usleep(400_000)
            let e = app.descendants(matching: .any)[id].firstMatch
            guard e.exists, e.frame == frame else { continue }
            let short = String(id.dropFirst("bubble:".count))
            let name = inRow(app, short, "bubble-name")
            if name.exists, name.frame.contains(point) { continue }
            let avatar = inRow(app, short, "sender-avatar")
            if avatar.exists, avatar.frame.contains(point) { continue }
            return (short, point)
        }
        return nil
    }

    /// Open the bar, drag the result list `scrollUp` times toward older hits, then aim at
    /// whichever hit is reachable there. Everything here is harness, not finding: kept apart
    /// from the landing so the symptom arm's expected-failure wrapper cannot swallow a broken
    /// rig — it did exactly that once, reporting a run whose app died at launch as a
    /// reproduction.
    private func aimed(scrollUp: Int, allowGrow: Bool)
        -> (app: XCUIApplication, screen: CGRect, id: String, point: CGPoint)? {
        let app = openSearch(allowGrow: allowGrow)
        let screen = app.windows.firstMatch.frame
        for _ in 0..<scrollUp { nudge(app, band: reachable(app, screen: screen)) }
        for _ in 0..<4 {
            if let hit = aimable(app, screen: screen) { return (app, screen, hit.id, hit.point) }
            nudge(app, band: reachable(app, screen: screen))
        }
        XCTFail("no reachable hit after \(scrollUp) drags up the result list "
                + "(results: \(describe(bubbles(app))))")
        return nil
    }

    /// Tap the aimed point and say where the transcript came to rest. Identical for every
    /// case but how far up the result list it has scrolled and whether the near-top grow may
    /// run inside the reveal.
    private func landing(_ arm: String,
                         _ aimed: (app: XCUIApplication, screen: CGRect, id: String, point: CGPoint)) {
        let (app, screen, id, point) = aimed
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

    /// A whole case, with the landing allowed to fail or not. `expectShort` marks an arm the
    /// symptom is being hunted in: the landing is recorded rather than asserted, because a
    /// lane that has not yet reproduced a thing cannot also claim how reliably it fires.
    /// Launching and aiming stay outside the expectation, so a broken rig goes red there.
    private func run(scrollUp: Int, allowGrow: Bool, expectShort: Bool = false) {
        let arm = "up\(scrollUp)/\(allowGrow ? "grow-allowed" : "grow-held")"
        guard let a = aimed(scrollUp: scrollUp, allowGrow: allowGrow) else { return }
        guard expectShort else { return landing(arm, a) }
        continueAfterFailure = true
        XCTExpectFailure("hunting the short landing (#2404)", strict: false) { landing(arm, a) }
    }

    /// The control: the reader opens the bar and taps a result without scrolling the list.
    /// The transcript is already at its end, so the reveal animates down from the tail. Both
    /// landings measured so far were this, and both were correct.
    func testAResultTappedWithoutScrollingTheListLands() {
        run(scrollUp: 0, allowGrow: true)
    }

    /// The reader scrolls back through the results before tapping one — the ordinary way of
    /// hunting something that is not the most recent mention of it, and what #2404 describes.
    /// The reveal then starts from wherever the result list was standing, which is the one
    /// thing the first run could not vary: both of its cases began at the same offset, 8676,
    /// and produced the same trajectory to the same landing. This arm moves that number.
    func testAResultTappedAfterScrollingBackThroughTheResults() {
        run(scrollUp: 14, allowGrow: true, expectShort: true)
    }

    /// The same, again, because a landing that is right once says nothing about one that is
    /// wrong sometimes. Two attempts is not a rate, but two is more than one.
    func testAResultTappedAfterScrollingBackThroughTheResultsAgain() {
        run(scrollUp: 14, allowGrow: true, expectShort: true)
    }

    /// The same scroll-back with the window held still for the length of the reveal. It is
    /// only worth reading if an arm above went short.
    func testAResultTappedAfterScrollingBackWithTheGrowHeld() {
        run(scrollUp: 14, allowGrow: false, expectShort: true)
    }
}
