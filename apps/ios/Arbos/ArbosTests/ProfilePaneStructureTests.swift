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

/// #1545 (Jacob, 2026-10-23): after "Block user" the row kept reading Block user. The
/// person pane's footer now derives from the relay's block list, so it flips to Unblock
/// once the list names the person and back once it no longer does. The Mac twin is
/// `crates/arbos-mac/src/app.rs` `block_state_tests`.
final class BlockRowTests: XCTestCase {
    private func person(_ id: String, _ handle: String) -> Person { Person(id: id, handle: handle) }

    func testBlockedMatchesByIdThenHandle() {
        let ken = person("gh_1", "kenobijon")
        XCTAssertNotNil(BlockRow.entry(in: [ken], id: ken.id, handle: ken.handle))
        // The pane may open from a row that only knows the handle.
        XCTAssertNotNil(BlockRow.entry(in: [ken], id: "", handle: "KenobiJon"))
        XCTAssertNil(BlockRow.entry(in: [ken], id: "gh_2", handle: "sami"))
        XCTAssertNil(BlockRow.entry(in: [], id: ken.id, handle: ken.handle))
    }

    func testBlockRowFlipsWithTheList() {
        let ken = person("gh_1", "kenobijon")
        var list: [Person] = []
        XCTAssertEqual(BlockRow.title(blocked: BlockRow.entry(in: list, id: ken.id, handle: ken.handle) != nil), "Block user")
        list.append(ken)
        XCTAssertEqual(BlockRow.title(blocked: BlockRow.entry(in: list, id: ken.id, handle: ken.handle) != nil), "Unblock")
        list.removeAll { $0.id == ken.id }
        XCTAssertEqual(BlockRow.title(blocked: BlockRow.entry(in: list, id: ken.id, handle: ken.handle) != nil), "Block user")
    }
}
