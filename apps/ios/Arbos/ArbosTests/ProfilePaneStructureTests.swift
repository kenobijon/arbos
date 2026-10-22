import XCTest
@testable import Arbos

/// The shared profile pane (D93): every kind lays out hero, then the tab
/// pill, then the selected tab's options (then a footer when it has one) —
/// kinds differ in their tab sets, not in that structure (Jacob, 2026-10-22).
/// The Mac twin is `crates/arbos-mac/src/ui/profiles/pane.rs` `tests`.
final class ProfilePaneStructureTests: XCTestCase {
    func testEveryKindLaysOutHeroThenPillThenOptions() {
        for (kind, tabs) in PaneKinds.all {
            XCTAssertGreaterThanOrEqual(tabs.count, 2, "\(kind): a pane has at least two tabs")
            XCTAssertEqual(ProfilePane.layout(tabs: tabs.count, footerSections: 1), [.hero, .tabPill, .options, .footer], kind)
            XCTAssertEqual(ProfilePane.layout(tabs: tabs.count, footerSections: 0), [.hero, .tabPill, .options], kind)
        }
        // One tab: no pill, the options follow the hero directly.
        XCTAssertEqual(ProfilePane.layout(tabs: 1, footerSections: 0), [.hero, .options])
    }

    func testKindsKeepTheirOwnTabSets() {
        XCTAssertEqual(PaneKinds.agent, ["Profile", "Settings", "Library"])
        XCTAssertEqual(PaneKinds.ownProfile, ["Profile", "Settings"])
        XCTAssertEqual(PaneKinds.person, ["Links", "Groups"])
        XCTAssertEqual(PaneKinds.group, ["Members", "Library"])
        XCTAssertEqual(PaneKinds.server, ["Channels", "Members", "Settings"])
        XCTAssertEqual(PaneKinds.channel, ["Profile", "Settings"])
        for (kind, tabs) in PaneKinds.all {
            XCTAssertEqual(Set(tabs).count, tabs.count, "\(kind): a tab label appears once")
        }
    }
}
