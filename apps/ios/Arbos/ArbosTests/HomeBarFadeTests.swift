import XCTest
@testable import Arbos

/// Home's floating bars ([feedback iphone-home-bar-fades-2026-10-28], Jacob 2026-10-28 18:21 UTC):
/// the discs + tabs at the top and the search capsule at the bottom float over the list like Grok
/// Bot's; rows stay visible and fade out under them instead of being cut. Geometry from
/// `ListTopFade.spec(.home)` and `HomeBottomFade.spec`, the same the layers draw from.
final class HomeBarFadeTests: XCTestCase {
    /// iPhone 17 / Air status bar (Dynamic Island) and the SE's.
    let statusBars: [CGFloat] = [59, 62, 20]
    /// Under the canvas at the bottom: the bar + the home indicator (34 / 0), or + the keyboard.
    let belowBar: [CGFloat] = [34, 0, 336]

    private func alpha(_ knots: [ListTopFade.Spec.Knot], at y: CGFloat) -> Double {
        // The mask's alpha at `y`: linear between the knots around it.
        guard let i = knots.firstIndex(where: { $0.y >= y }) else { return knots.last?.alpha ?? 0 }
        if i == 0 || knots[i].y == y { return knots[i].alpha }
        let a = knots[i - 1], b = knots[i]
        return a.alpha + (b.alpha - a.alpha) * Double((y - a.y) / (b.y - a.y))
    }

    func testTopBarsFloat_rowsReadUnderTheDiscsAndTabsAndAreGoneWhereTheFirstRowRests() {
        for status in statusBars {
            let spec = ListTopFade.spec(.home, statusBar: status)
            XCTAssertEqual(spec.topInset, 0)
            // Opaque through the status bar and `topSolid` past it — never a row behind the status bar.
            XCTAssertEqual(alpha(spec.knots, at: status), 1, "@\(status)")
            XCTAssertEqual(spec.solid, status + HomeChrome.topSolid)
            // Under the disc row the rows read, dimmed: the wash is below 1 and at least 3/4 at its bottom edge.
            let discsBottom = status + HomeChrome.discsBottom
            XCTAssertEqual(alpha(spec.knots, at: discsBottom), HomeChrome.discsWash, accuracy: 0.001)
            XCTAssertLessThan(HomeChrome.discsWash, 1)
            XCTAssertGreaterThanOrEqual(HomeChrome.discsWash, 0.7)
            // Under the tabs: still over half, so the words stay legible.
            let tabsBottom = discsBottom + HomeChrome.filterBand
            XCTAssertEqual(alpha(spec.knots, at: tabsBottom), HomeChrome.tabsWash, accuracy: 0.001)
            XCTAssertGreaterThan(HomeChrome.tabsWash, 0.5)
            XCTAssertLessThan(HomeChrome.tabsWash, HomeChrome.discsWash)
            // Gone where the first row rests, and the layer ends there.
            XCTAssertEqual(alpha(spec.knots, at: status + HomeChrome.listTop), 0)
            XCTAssertEqual(spec.height, status + HomeChrome.listTop)
            // Monotone: never brighter further down.
            for (a, b) in zip(spec.knots, spec.knots.dropFirst()) {
                XCTAssertLessThanOrEqual(b.alpha, a.alpha); XCTAssertGreaterThanOrEqual(b.y, a.y)
            }
            XCTAssertEqual(spec.stops.count, spec.knots.count)
        }
    }

    func testSearchModeKeepsTheSameFloatingDiscsWithoutTabs() {
        let status: CGFloat = 59
        let spec = ListTopFade.spec(.homeSearch, statusBar: status)
        XCTAssertEqual(spec.solid, status + HomeChrome.topSolid)
        XCTAssertEqual(alpha(spec.knots, at: status + HomeChrome.discsBottom), HomeChrome.discsWash, accuracy: 0.001)
        XCTAssertEqual(spec.height, status + HomeChrome.barOnlyListTop)
    }

    func testBottomCapsuleFloats_rowsReadUnderItAndAreGoneAtTheHomeIndicator() {
        XCTAssertEqual(HomeChrome.bottomBar, Controls.height + HomeChrome.bottomPad)
        for below in belowBar {
            let spec = HomeBottomFade.spec(bottomInset: HomeChrome.bottomBar + below)
            XCTAssertEqual(spec.fade, HomeChrome.bottomFade)
            XCTAssertEqual(spec.height, HomeChrome.bottomFade + HomeChrome.bottomBar + below, "+\(below)")
            // Clear at the top of the fade, under half at the capsule's top edge, nearly full at its
            // bottom edge, full from the bar's padding through the home indicator (or keyboard).
            XCTAssertEqual(alpha(spec.knots, at: 0), 0)
            XCTAssertEqual(alpha(spec.knots, at: spec.fade), HomeChrome.capsuleTopWash, accuracy: 0.001)
            XCTAssertGreaterThan(HomeChrome.capsuleTopWash, 0.5)
            XCTAssertEqual(alpha(spec.knots, at: spec.fade + Controls.height), HomeChrome.capsuleBottomWash, accuracy: 0.001)
            XCTAssertGreaterThanOrEqual(HomeChrome.capsuleBottomWash, 0.9)
            XCTAssertEqual(alpha(spec.knots, at: spec.fade + HomeChrome.bottomBar), 1)
            XCTAssertEqual(alpha(spec.knots, at: spec.height), 1)
            for (a, b) in zip(spec.knots, spec.knots.dropFirst()) {
                XCTAssertGreaterThanOrEqual(b.alpha, a.alpha); XCTAssertGreaterThanOrEqual(b.y, a.y)
            }
            XCTAssertEqual(spec.stops.count, spec.knots.count)
            XCTAssertEqual(spec.stops.last?.location, 1)
        }
    }

    func testFadeHeightsAreShortOnThePixelAndTheListsClearThem() {
        // Jacob's chat band set the family's range (24–40); Home's bottom fade is in it.
        XCTAssertGreaterThanOrEqual(HomeChrome.bottomFade, 24)
        XCTAssertLessThanOrEqual(HomeChrome.bottomFade, 40)
        XCTAssertEqual(HomeChrome.bottomFade * 3, (HomeChrome.bottomFade * 3).rounded(), accuracy: 0.001)
        XCTAssertEqual(HomeChrome.topSolid * 3, (HomeChrome.topSolid * 3).rounded(), accuracy: 0.001)
        // The lists' trailing clear row is the fade: the last row at rest sits above it. The
        // leading clear row is the whole top layer: the first row at rest sits below it.
        XCTAssertGreaterThanOrEqual(HomeChrome.listBottom, HomeChrome.bottomFade)
        XCTAssertEqual(HomeChrome.listTop, ListTopFade.spec(.home, statusBar: 0).height)
        XCTAssertEqual(HomeChrome.barOnlyListTop, ListTopFade.spec(.homeSearch, statusBar: 0).height)
    }

    func testMatteAndGlassShareTheGeometryAndDifferOnlyInMaterial() {
        // One geometry; the material's own alpha is the surface's (page colour / wash over material).
        XCTAssertEqual(ComposerBottomBand.wash(glass: false), 1)
        XCTAssertEqual(ComposerBottomBand.wash(glass: true), ListTopFade.glassWash)
    }
}
