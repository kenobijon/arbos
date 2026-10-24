//! The command palette's result model (feedback `search-palette-2026-10-24`,
//! Jacob): ⌘⇧F / ⌘K / the sidebar's Search disc open one centred palette over
//! the window — Grok Bot's — that finds everything: agents, people (DMs),
//! groups, servers and their channels, and the files, pictures and links
//! shared in chats. The ranked name/handle matching of #1477
//! ([`crate::search`]) stays the matcher for chat rows; this module decides
//! what the rows are, how they group and in what order they come.
//!
//! Sections, top to bottom: **People & agents** (a DM or agent found by its
//! name / handle), **Chats** (groups, a member, a chat title, a message),
//! **Servers & channels**, **Files, media & links**. Inside a section newest
//! activity first. With no query the palette shows **Recent** — every chat
//! and server by activity — and the first nine rows carry ⌘1–⌘9, like Grok's.

use crate::model::Id;

/// What a row is; decides its glyph and its section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Agent,
    Person,
    Group,
    Server,
    Channel,
    Media,
    File,
    Link,
}

/// The group a row is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Recent,
    People,
    Chats,
    Servers,
    Content,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Recent => "Recent",
            Section::People => "People & agents",
            Section::Chats => "Chats",
            Section::Servers => "Servers & channels",
            Section::Content => "Files, media & links",
        }
    }
}

/// What opening a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Open the chat (agent, DM, group).
    Space(Id),
    /// Open the chat and scroll to the message that matched.
    Message { space_id: Id, chat_id: Id, message_id: Id },
    /// Enter the server (its `#general`).
    Server(Id),
    /// Open one channel of a server.
    Channel { server_id: Id, channel_id: Id },
    /// Open a picture / video / file in the media viewer.
    Media { chat_id: Id, message_id: Id, part_id: Id },
    /// Open a link in the browser.
    Link(String),
}

/// One palette row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub title: String,
    pub subtitle: String,
    /// Newest activity (ms); orders rows inside a section.
    pub when: u64,
    /// Whether the row matched by something other than its own name — a
    /// member, a chat title, a message. Name hits come first in a section.
    pub indirect: bool,
    pub target: Target,
}

impl Entry {
    /// Which section this row lists under when a query is typed.
    pub fn section(&self) -> Section {
        match (self.kind, self.indirect) {
            (Kind::Agent | Kind::Person, false) => Section::People,
            (Kind::Agent | Kind::Person, true) => Section::Chats,
            (Kind::Group, _) => Section::Chats,
            (Kind::Server | Kind::Channel, _) => Section::Servers,
            (Kind::Media | Kind::File | Kind::Link, _) => Section::Content,
        }
    }
}

/// Rows per section with a query (the list scrolls, but a long tail of
/// message hits would bury the servers and files).
pub const SECTION_CAP: usize = 8;
/// Rows on the empty query (recent chats and servers by activity).
pub const RECENT_CAP: usize = 24;
/// How many rows carry a ⌘1–⌘9 shortcut.
pub const SHORTCUTS: usize = 9;

/// Group and order `entries` for the list. Empty `query`: one **Recent**
/// section by activity. Otherwise the four sections in order, each with its
/// direct (name) hits first, newest first, capped at [`SECTION_CAP`].
pub fn arrange(mut entries: Vec<Entry>, query_is_empty: bool) -> Vec<(Section, Vec<Entry>)> {
    if query_is_empty {
        entries.retain(|e| matches!(e.kind, Kind::Agent | Kind::Person | Kind::Group | Kind::Server));
        entries.sort_by_key(|e| std::cmp::Reverse(e.when));
        entries.truncate(RECENT_CAP);
        return if entries.is_empty() {
            Vec::new()
        } else {
            vec![(Section::Recent, entries)]
        };
    }
    let mut out: Vec<(Section, Vec<Entry>)> = Vec::new();
    for section in [
        Section::People,
        Section::Chats,
        Section::Servers,
        Section::Content,
    ] {
        let mut rows: Vec<Entry> = entries
            .iter()
            .filter(|e| e.section() == section)
            .cloned()
            .collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by_key(|e| (e.indirect, std::cmp::Reverse(e.when)));
        rows.truncate(SECTION_CAP);
        out.push((section, rows));
    }
    out
}

/// The rows in list order, flattened across sections (what ↑↓ and ⌘1–9 walk).
pub fn flatten(arranged: &[(Section, Vec<Entry>)]) -> Vec<&Entry> {
    arranged.iter().flat_map(|(_, rows)| rows.iter()).collect()
}

/// "⌘1" … "⌘9" for the first nine rows; `None` after that.
pub fn shortcut_label(index: usize) -> Option<String> {
    (index < SHORTCUTS).then(|| format!("⌘{}", index + 1))
}

/// Keyboard state of the open palette: which row is highlighted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub selected: usize,
}

impl Cursor {
    /// ↑ / ↓ by `delta` over `len` rows: clamps at both ends (Grok's palette
    /// does not wrap).
    pub fn step(&mut self, delta: i32, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as i64 + delta as i64;
        self.selected = next.clamp(0, len as i64 - 1) as usize;
    }

    /// The list changed (typing): keep the highlight inside it.
    pub fn clamp(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }
}

/// "Server · 3 channels · 12 members".
pub fn server_subtitle(channels: usize, members: u64) -> String {
    let mut s = format!("Server · {channels} channel{}", if channels == 1 { "" } else { "s" });
    if members > 0 {
        s.push_str(&format!(" · {members} member{}", if members == 1 { "" } else { "s" }));
    }
    s
}

/// "Group · 4 members".
pub fn group_subtitle(members: usize) -> String {
    format!("Group · {members} member{}", if members == 1 { "" } else { "s" })
}

/// The host of a link for its subtitle ("github.com"), the URL itself when
/// it has none.
pub fn link_host(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, title: &str, when: u64, indirect: bool) -> Entry {
        Entry {
            kind,
            title: title.into(),
            subtitle: String::new(),
            when,
            indirect,
            target: Target::Space(format!("id-{title}")),
        }
    }

    fn fixture() -> Vec<Entry> {
        vec![
            entry(Kind::Agent, "Arbos", 50, false),
            entry(Kind::Person, "Carro", 40, false),
            entry(Kind::Group, "Testing", 60, false),
            // A DM found through a message body: a Chats row, not a person row.
            entry(Kind::Person, "Sam", 70, true),
            entry(Kind::Server, "Launch Crew", 30, false),
            Entry {
                target: Target::Channel {
                    server_id: "s1".into(),
                    channel_id: "c1".into(),
                },
                ..entry(Kind::Channel, "#general", 80, false)
            },
            Entry {
                target: Target::Media {
                    chat_id: "ch".into(),
                    message_id: "m".into(),
                    part_id: "p".into(),
                },
                ..entry(Kind::File, "plan.pdf", 90, false)
            },
            Entry {
                target: Target::Link("https://www.github.com/unarbos/arbos".into()),
                ..entry(Kind::Link, "github.com/unarbos/arbos", 20, false)
            },
        ]
    }

    /// People/agents → chats → servers/channels → files/media/links, each
    /// section newest first; a server *and* its channel both appear.
    #[test]
    fn sections_come_in_jacobs_order_with_a_channel_and_a_file() {
        let arranged = arrange(fixture(), false);
        let sections: Vec<Section> = arranged.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            sections,
            vec![
                Section::People,
                Section::Chats,
                Section::Servers,
                Section::Content
            ]
        );
        let titles = |s: Section| -> Vec<String> {
            arranged
                .iter()
                .find(|(x, _)| *x == s)
                .map(|(_, rows)| rows.iter().map(|r| r.title.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(titles(Section::People), ["Arbos", "Carro"]);
        assert_eq!(titles(Section::Chats), ["Testing", "Sam"]);
        assert_eq!(titles(Section::Servers), ["#general", "Launch Crew"]);
        assert_eq!(titles(Section::Content), ["plan.pdf", "github.com/unarbos/arbos"]);
        assert!(flatten(&arranged)
            .iter()
            .any(|e| matches!(e.target, Target::Channel { .. })));
        assert!(flatten(&arranged)
            .iter()
            .any(|e| matches!(e.target, Target::Media { .. })));
    }

    /// Empty query: recent chats and servers by activity, no files or links,
    /// ⌘1–⌘9 on the first nine rows and nothing after.
    #[test]
    fn the_empty_query_shows_recents_with_shortcuts() {
        let arranged = arrange(fixture(), true);
        assert_eq!(arranged.len(), 1);
        assert_eq!(arranged[0].0, Section::Recent);
        let titles: Vec<&str> = arranged[0].1.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["Sam", "Testing", "Arbos", "Carro", "Launch Crew"]);
        assert_eq!(shortcut_label(0).as_deref(), Some("⌘1"));
        assert_eq!(shortcut_label(8).as_deref(), Some("⌘9"));
        assert_eq!(shortcut_label(9), None);
    }

    /// A name hit outranks an indirect hit inside a section, whatever the age.
    #[test]
    fn name_hits_come_before_member_and_message_hits() {
        let rows = vec![
            entry(Kind::Group, "Old by name", 10, false),
            entry(Kind::Group, "New by member", 99, true),
        ];
        let arranged = arrange(rows, false);
        let titles: Vec<&str> = arranged[0].1.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["Old by name", "New by member"]);
    }

    #[test]
    fn a_section_is_capped() {
        let rows: Vec<Entry> = (0..20)
            .map(|i| entry(Kind::Person, &format!("p{i}"), i, false))
            .collect();
        let arranged = arrange(rows, false);
        assert_eq!(arranged[0].1.len(), SECTION_CAP);
        assert_eq!(arranged[0].1[0].title, "p19", "newest first");
    }

    /// ↑↓ clamp at the ends; a shorter list pulls the highlight back in.
    #[test]
    fn the_cursor_steps_and_clamps() {
        let mut c = Cursor::default();
        c.step(-1, 5);
        assert_eq!(c.selected, 0);
        c.step(1, 5);
        c.step(1, 5);
        assert_eq!(c.selected, 2);
        c.step(10, 5);
        assert_eq!(c.selected, 4);
        c.clamp(2);
        assert_eq!(c.selected, 1);
        c.clamp(0);
        assert_eq!(c.selected, 0);
        c.step(1, 0);
        assert_eq!(c.selected, 0);
    }

    #[test]
    fn subtitles_read_naturally() {
        assert_eq!(server_subtitle(1, 1), "Server · 1 channel · 1 member");
        assert_eq!(server_subtitle(3, 0), "Server · 3 channels");
        assert_eq!(group_subtitle(4), "Group · 4 members");
        assert_eq!(link_host("https://www.github.com/unarbos/arbos?x=1"), "github.com");
        assert_eq!(link_host("http://relay.arbos.life"), "relay.arbos.life");
    }
}
