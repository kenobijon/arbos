import XCTest
@testable import Arbos

/// The New Message sheet opens in its final layout (Jacob, iPhone, 2026-10-22):
/// the directory is restored from the store and refreshed ahead of the sheet,
/// an unchanged refresh never repaints it, and the create rows share one left
/// edge with the people rows. Mirrors crates/arbos-mac `people::first_paint_tests`.
final class NewMessageSheetTests: XCTestCase {
    private func person(_ id: String, _ handle: String, avatar: String? = nil) -> Person {
        Person(id: id, handle: handle, avatarUrl: avatar, name: handle.capitalized, status: "member", avatarColorIndex: 3)
    }

    private var sample: [Person] {
        [person("gh_1", "allan", avatar: "https://avatars.githubusercontent.com/u/1?v=4"),
         person("gh_2", "alw-one"),
         person("test_3", "arbosdemo-alice")]
    }

    func testCreateRowsAndPeopleRowsShareOneLeftEdge() {
        // Card 16 in, row content 14 in, a 44 pt glyph / picture, 14 to the text:
        // the same numbers for New agent / New group / New server and every person row.
        XCTAssertEqual(SheetRow.cardInset, 16)
        XCTAssertEqual(SheetRow.paddingX, 14)
        XCTAssertEqual(SheetRow.avatar, 44)
        XCTAssertEqual(SheetRow.gap, 14)
        XCTAssertEqual(SheetRow.textInset, 72, "divider and text start 72 pt into the card")
        XCTAssertEqual(SheetRow.textLeading, 88, "text column 88 pt from the sheet edge")
    }

    func testDirectoryCacheRoundTrip() throws {
        let data = try XCTUnwrap(AppModel.encodeDirectoryCache(sample))
        XCTAssertEqual(AppModel.decodeDirectoryCache(data), sample)
        XCTAssertNil(AppModel.encodeDirectoryCache([]), "nothing to restore from an empty list")
        XCTAssertNil(AppModel.decodeDirectoryCache(Data("[]".utf8)))
        XCTAssertNil(AppModel.decodeDirectoryCache(Data("not json".utf8)))
    }

    func testRestoredDirectoryIsRefreshedInTheBackgroundThenHonoursTheTtl() {
        let ttl = AppModel.directoryTtlMs
        // Nothing loaded yet: fetch.
        XCTAssertTrue(AppModel.directoryNeedsRefresh(loadedAt: 0, now: 5_000, isEmpty: true))
        // Restored from the store, no fetch this launch: refetch (behind the painted list).
        XCTAssertTrue(AppModel.directoryNeedsRefresh(loadedAt: 0, now: 5_000, isEmpty: false))
        // Fetched a minute ago: the sheet's own `.task` is a no-op.
        XCTAssertFalse(AppModel.directoryNeedsRefresh(loadedAt: 100_000, now: 160_000, isEmpty: false))
        XCTAssertFalse(AppModel.directoryNeedsRefresh(loadedAt: 100_000, now: 100_000 + ttl - 1, isEmpty: false))
        XCTAssertTrue(AppModel.directoryNeedsRefresh(loadedAt: 100_000, now: 100_000 + ttl, isEmpty: false))
        // An empty answer never counts as fresh.
        XCTAssertTrue(AppModel.directoryNeedsRefresh(loadedAt: 100_000, now: 100_001, isEmpty: true))
    }

    func testUnchangedRefreshDoesNotRepublish() {
        XCTAssertNil(AppModel.directoryUpdate(current: sample, fresh: sample), "same list: nothing to repaint")
        XCTAssertEqual(AppModel.directoryUpdate(current: [], fresh: sample), sample)
        var renamed = sample
        renamed[1].name = "Al W."
        XCTAssertEqual(AppModel.directoryUpdate(current: sample, fresh: renamed), renamed)
        XCTAssertEqual(AppModel.directoryUpdate(current: sample, fresh: Array(sample.dropLast())), Array(sample.dropLast()))
    }

    func testAvatarWarmupCoversTheFirstScreensOnly() {
        XCTAssertEqual(AvatarImageCache.warmLimit, 48)
        XCTAssertEqual(AvatarImageCache.maxPixelSize, 384, "128 pt hero at 3x")
    }
}
