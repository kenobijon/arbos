import ArbosFFI
import XCTest
@testable import Arbos

/// Jacob, 2026-10-26 ([feedback search-settings-ios-2026-10-26]): the bottom
/// search also finds Settings — every page of the Settings list, its sub-pages
/// and the words people type for them, through the shared index
/// (`arbos_core::settings_search` over ArbosFFI). A Settings section lists
/// after Servers & channels and before Messages, first of all when the query
/// *is* a title or keyword; a title hit says where the page lives, a keyword
/// hit names the keyword; Model · Reasoning · Voice come once per owned agent.
final class SearchSettingsTests: XCTestCase {
    private var agents: [GlobalSearch.AgentRow] {
        [
            GlobalSearch.AgentRow(projectId: "p-arbos", name: "Arbos", model: "gpt-6.1-sol", reasoning: "High", voice: "Default"),
            GlobalSearch.AgentRow(projectId: "p-comms", name: "Comms", model: "gpt-6.1-sol", reasoning: "High", voice: "Rex"),
            GlobalSearch.AgentRow(projectId: "p-affine", name: "Affine", model: "gpt-6.1-sol", reasoning: "High", voice: "Ara"),
        ]
    }

    private func entry(_ kind: GlobalSearch.Kind, _ title: String, when: Int64, rank: GlobalSearch.Rank,
                       target: GlobalSearch.Target) -> GlobalSearch.Entry {
        GlobalSearch.Entry(kind: kind, title: title, subtitle: "", when: when, rank: rank, target: target)
    }

    private func titles(_ groups: [GlobalSearch.Group], _ section: GlobalSearch.Section) -> [String] {
        groups.first { $0.section == section }?.rows.map(\.title) ?? []
    }

    // MARK: Ranking

    /// "dev": Devin (a name hit) stays first; Devices follows in its own section,
    /// after Servers & channels and before Messages.
    func testPrefixHitListsAfterPeopleChatsAndServers() {
        let q = HomeSearch.normalize("dev")
        var rows = [
            entry(.person, "Devin", when: 10, rank: .of(name: "Devin", handle: "devin", q: q), target: .people("dm-1")),
            entry(.server, "Dev Crew", when: 5, rank: .of(name: "Dev Crew", handle: "", q: q), target: .server("s1")),
            entry(.message, "Arbos", when: 99, rank: .indirect, target: .message(chatId: "c", projectId: "p", messageId: "m")),
        ]
        rows += GlobalSearch.settingsEntries(query: q, agents: agents)
        let groups = GlobalSearch.arrange(rows, queryIsEmpty: false)
        XCTAssertEqual(groups.map(\.section), [.people, .servers, .settings, .messages])
        XCTAssertEqual(titles(groups, .settings), ["Devices"])
        let devices = groups[2].rows[0]
        XCTAssertEqual(devices.subtitle, "Settings")
        XCTAssertEqual(devices.target, .setting(.page(.devices)))
        XCTAssertEqual(devices.picture, .settingsTile("laptopcomputer"))
    }

    /// "opacity": no page is called that; Appearance's keyword is an exact hit, so
    /// Settings jumps first and the keyword leads the subtitle.
    func testExactKeywordPutsSettingsFirstAndNamesTheKeyword() {
        let q = HomeSearch.normalize("Opacity")
        var rows = [entry(.message, "Affine", when: 99, rank: .indirect, target: .message(chatId: "c", projectId: "p", messageId: "m"))]
        rows += GlobalSearch.settingsEntries(query: q, agents: agents)
        let groups = GlobalSearch.arrange(rows, queryIsEmpty: false)
        XCTAssertEqual(groups.map(\.section), [.settings, .messages])
        let appearance = groups[0].rows[0]
        XCTAssertEqual(appearance.title, "Appearance")
        XCTAssertEqual(appearance.subtitle, "Opacity · Settings")
        XCTAssertEqual(appearance.target, .setting(.page(.appearance)))
        // Jacob's other examples.
        XCTAssertEqual(GlobalSearch.settingsEntries(query: "theme", agents: []).first?.target, .setting(.page(.appearance)))
        XCTAssertEqual(GlobalSearch.settingsEntries(query: "token", agents: []).first?.target, .setting(.page(.secrets)))
        XCTAssertEqual(GlobalSearch.settingsEntries(query: "1Password", agents: []).first?.target, .setting(.page(.secrets)))
    }

    /// "voice": one row per owned agent in the order given (open chat's agent
    /// first), each with its current value; the title is exact, so first.
    func testAgentSettingsComeOncePerOwnedAgent() {
        let groups = GlobalSearch.arrange(GlobalSearch.settingsEntries(query: "voice", agents: agents), queryIsEmpty: false)
        XCTAssertEqual(groups.first?.section, .settings)
        let voice = groups[0].rows
        XCTAssertEqual(voice.count, 3)
        XCTAssertTrue(voice.allSatisfy { $0.title == "Voice" })
        XCTAssertEqual(voice.map(\.subtitle), ["Arbos · Settings · Default", "Comms · Settings · Rex", "Affine · Settings · Ara"])
        XCTAssertEqual(voice[0].target, .setting(.agent(projectId: "p-arbos", setting: .voice)))
        XCTAssertEqual(voice[0].picture, .project("p-arbos"))
        let model = GlobalSearch.settingsEntries(query: "model", agents: agents)
        XCTAssertEqual(model.first?.target, .setting(.agent(projectId: "p-arbos", setting: .model)))
        XCTAssertEqual(GlobalSearch.settingsEntries(query: "reasoning", agents: agents).first?.target,
                       .setting(.agent(projectId: "p-arbos", setting: .reasoning)))
        XCTAssertTrue(GlobalSearch.settingsEntries(query: "voice", agents: []).isEmpty, "no owned agents, no rows")
    }

    /// Title hits before keyword hits; a section caps at eight like every other.
    func testTitleHitsOutrankKeywordHitsAndNothingShowsWithoutAQuery() {
        let about = GlobalSearch.settingsEntries(query: "about", agents: [])
        XCTAssertEqual(about.first?.target, .setting(.page(.about)))
        XCTAssertTrue(GlobalSearch.settingsEntries(query: "", agents: agents).isEmpty)
        let recent = GlobalSearch.arrange(GlobalSearch.settingsEntries(query: "", agents: agents), queryIsEmpty: true)
        XCTAssertTrue(recent.isEmpty, "Settings never show in Recent")
        var many = agents
        for i in 0..<10 { many.append(GlobalSearch.AgentRow(projectId: "p-\(i)", name: "A\(i)", model: "", reasoning: "", voice: "")) }
        let capped = GlobalSearch.arrange(GlobalSearch.settingsEntries(query: "voice", agents: many), queryIsEmpty: false)
        XCTAssertEqual(capped[0].rows.count, GlobalSearch.sectionCap)
        XCTAssertEqual(capped[0].rows[0].subtitle, "Arbos · Settings · Default", "the open chat's agent stays first")
    }

    // MARK: Navigation targets and structure

    /// Every page the Settings list shows is reachable from search with its own
    /// route — and so are the Secrets forms, the feedback sheet and Profile.
    func testEveryListedSettingsPageIsIndexedWithARoute() {
        let index = settingsIndex()
        for page in SettingsView.listedPages {
            let routed = index.filter { GlobalSearch.settingsRoute(for: $0.item) == .page(page) }
            XCTAssertFalse(routed.isEmpty, "\(page) has no index entry")
            for e in routed {
                let found = GlobalSearch.settingsEntries(query: e.title, agents: [])
                XCTAssertTrue(found.contains { $0.target == .setting(.page(page)) }, "\(page) is not found by its title \(e.title)")
            }
        }
        let find = { (typed: String) in GlobalSearch.settingsEntries(query: typed, agents: []).map(\.target) }
        XCTAssertTrue(find("new secret").contains(.setting(.page(.newSecret))))
        XCTAssertTrue(find("connect 1password").contains(.setting(.page(.connectOnePassword))))
        XCTAssertTrue(find("feedback").contains(.setting(.feedback)))
        XCTAssertTrue(find("profile").contains(.setting(.profile)))
        XCTAssertTrue(find("on this iphone").contains(.setting(.page(.root))))
        XCTAssertTrue(find("text size").contains(.setting(.page(.textSize))))
        // Every iPhone entry of the shared index maps to an iPhone route (the
        // per-agent rows map through the agent); the Mac-only Updates page is absent.
        for e in index where !e.perAgent {
            XCTAssertNotNil(GlobalSearch.settingsRoute(for: e.item), "\(e.item) has no iPhone route")
            XCTAssertGreaterThanOrEqual(e.keywords.count, 3, "\(e.item) has too few keywords")
        }
        XCTAssertFalse(index.contains { $0.item == .updates })
        XCTAssertTrue(find("updates").contains(.setting(.page(.about))), "the iPhone's updates live on About")
        // Not indexed on purpose: Sign out, Pause all secrets.
        XCTAssertTrue(find("sign out").isEmpty)
        XCTAssertTrue(find("pause").isEmpty)
        // Every route lands on a page word `SettingsView(landing:)` understands.
        for page in GlobalSearch.SettingsPageRoute.allCases {
            XCTAssertFalse(page.rawValue.isEmpty)
            XCTAssertFalse(GlobalSearch.settingsGlyph(.page(page)).isEmpty)
        }
    }

    /// The section sits between Servers & channels and Messages, and the label is the Mac's.
    func testSectionOrderAndLabel() {
        XCTAssertEqual(GlobalSearch.Section.withQuery, [.people, .chats, .servers, .settings, .messages, .content])
        XCTAssertEqual(GlobalSearch.Section.settings.label, "Settings")
        XCTAssertEqual(GlobalSearch.keywordLabel("opacity"), "Opacity")
    }
}
