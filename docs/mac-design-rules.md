# Mac app — layout rules that must not drift

These rules were decided by Jacob and have been lost once already (#1134 built
the compact strip; #1491 replaced it with an always-open Search pill; this page
and the tests in `crates/arbos-mac/src/ui/sidebar.rs` keep them in the repo).
Design-store sources: `docs/group-chat-redesign.md` and the approved mocks in
`media/feedback/sidebar-top-strip/` of the project store.

## Three-pane rule (app-wide, Jacob 2026-10-20)

Exactly three panes.

- **Left** always lists chats, groups and servers. At its top only three
  controls: **+**, **search**, **minimize** (collapse). Entering a server slides
  the left pane over to that server's channel list (back arrow returns).
- **Centre** is always a chat (agent, group or channel). Nothing else, ever.
- **Right** shows every details / members / admin / settings screen (user
  profiles, group settings + members, agent details, server settings, app
  settings). It opens beside the chat, never over it, and closes with an **X**.
- Every right-pane screen uses one layout: editable picture, editable name, a
  row of tabs, then lists or settings beneath.

Code: `sidebar.rs` module docs and `strip_controls()` (pinned by
`strip_tests::the_strip_holds_exactly_search_plus_collapse`); the right pane's
shared layout is `profiles/pane.rs` (`layout()` + every kind's `TABS`).

## Chat-list top strip (approved mock 2026-10-24, [feedback sidebar-strip-filter-2026-10-24])

Points at 1×; the Mac draws at 2 px/pt.

| Item | Value |
|---|---|
| Strip height | 44 (`HEADER_HEIGHT`), controls centred on the traffic lights' line (20.75) |
| Buttons, left to right | search · + · collapse — three round discs, right-aligned |
| Disc | 32 (`STRIP_BUTTON` = `CONTROL`), fill `surface_2`, glyph 15–16 in `text` |
| Gap / right inset | 10 (`STRIP_GAP`) / 12 (`STRIP_INSET`) |
| Search | closed by default; click or **⌘F** expands it into a 32 pt capsule from the strip's left edge to the "+"; Escape or the clear dot fold it; a typed query keeps it open |
| Inside a server | no search (spacer); "+" only for owner/admin; collapse always |
| Folded sidebar | the chat header shows the same round disc beside the lights (`header::sidebar_toggle`) |

## Chat-list filter (the iPhone home's `PaneTabs`)

Directly under the strip: **All · Agents · People · Servers** — the iPhone
`HomeListFilter` with the same labels and order. Bare labels, the selected one
on a capsule, centred; no track, no border. It stays while the search field is
open: the query narrows the list, the segment still scopes it.

| Item | Mac | iPhone |
|---|---|---|
| Capsule height / gap | 32 / 4 | 32 / 4 |
| Label | `TEXT_BODY` 15, semibold when selected | `Theme.body` 17, semibold when selected |
| Side padding | **10** (`FILTER_PAD_X`; four segments at 16 overflow the 296 pt default sidebar) | 16 |
| Selected fill / text | `bubble_assistant` / `text` | `bubbleAssistant` / `text` |
| Other labels | `text_muted` | `muted` |
| Scope | All: everything · Agents: my agents (not shared-in) · People: DMs + groups · Servers: servers only | same (`HomeView.homeRows`) |

Server rows draw their picture as a rounded square (corner 11/36 of the size,
`SERVER_PICTURE_CORNER`) so servers read apart from the agent and people discs.

## Message timestamps (Jacob 2026-10-24, [feedback timestamps-hover-only-2026-10-24])

**Message timestamps are hover-only, never always-on.** A bubble never carries
its time — not in a footer, not after the text, in no chat kind (1:1, group,
channel; mine, another person's, an agent's). The time ("9:41 PM") appears only
while the pointer is over the row, beside the bubble on its outer edge (left of
my bubble, after the ☺ ↩ ⋯ rail of everyone else's), fading with the rail; at
rest it is not drawn at all. On the iPhone the same rule is the iMessage
drag-to-reveal (`timeReveal`): no inline or footer time in any bubble.

Lost twice before this page existed: v0.6.183 (#1437, group bubbles got the time
after the text) and v0.6.203 (#1496, a tester's request for an always-visible
bottom-right footer time). The sent/seen label on my own bubbles is a separate
element and may stay in the footer.

Code: `transcript.rs` `hover_time()` (returns nothing at opacity 0) and
`bubble_footer()` (receipt only), pinned by
`layout_tests::the_time_is_hidden_at_rest_and_beside_the_bubble_on_hover`;
iPhone `Transcript.swift` `RowView` + `SenderNameRowTests`.
