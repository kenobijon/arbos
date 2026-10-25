import XCTest
@testable import Arbos

/// The avatar picker offers the same four controls to every target (Jacob, 2026-10-24: the
/// group / server picker had only Bot / Upload / Reset). My own picture (Jacob, 2026-10-25)
/// opens the same sheet without the leaf tab: Generate | Upload, Reset in the bar.
final class AvatarPickerTabsTests: XCTestCase {
    func testEveryTargetGetsBotGenerateUploadAndReset() {
        for target in [AvatarTarget.agent("a"), .group("g"), .server("s")] {
            XCTAssertEqual(AvatarPickerSheet.tabs(for: target), [.bot, .generate, .upload], "\(target)")
        }
        XCTAssertEqual(AvatarPickerTab.allCases.count, 3)
    }

    func testMyProfileGetsGenerateAndUploadWithoutALeafTab() {
        let tabs = AvatarPickerSheet.tabs(for: .me)
        XCTAssertEqual(tabs, [.generate, .upload])
        XCTAssertFalse(tabs.contains(.bot), "people are coloured initials, not leaves")
        XCTAssertEqual(tabs.first, .generate, "Generate is the landing tab")
    }
}
