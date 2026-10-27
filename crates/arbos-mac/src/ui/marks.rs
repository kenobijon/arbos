//! Status marks that are not coloured dots (Jacob, 2026-10-27, [feedback
//! neon-dots-mac-2026-10-27]: "replace all neon glimmer icons like this in the
//! app with braille spinners always, on a matter of principle never use
//! colored dots"). Activity → the braille spinner (`ui::working`); unread →
//! the count pill; a fault → the red `exclamation` glyph; a resting state
//! (awake, asleep, read) → nothing. The source lint (`lint_tests`) refuses a
//! new small round `div` filled with a status colour anywhere in this crate.

use gpui::{div, prelude::*, px, AnyElement, App, Div, FontWeight};

use crate::model::MachineState;
use crate::ui::icons::icon;
use crate::ui::theme::Theme;
use crate::ui::working;

/// The count pill: an accent capsule with white digits (Grok Bot's unread
/// mark). `UNREAD_PILL_H` tall, at least as wide as it is tall, "99+" past 99.
pub const UNREAD_PILL_H: f32 = 16.0;
pub const UNREAD_PILL_TEXT: f32 = 10.5;

pub fn unread_label(count: usize) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.max(1).to_string()
    }
}

pub fn unread_pill(count: usize, theme: &Theme) -> Div {
    div()
        .h(px(UNREAD_PILL_H))
        .min_w(px(UNREAD_PILL_H))
        .px(px(5.0))
        .rounded_full()
        .bg(theme.accent)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(UNREAD_PILL_TEXT))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(gpui::white())
        .debug_selector(|| "unread-pill".into())
        .child(unread_label(count))
}

/// What a machine state shows instead of its old coloured dot: the spinner
/// while it wakes, the red `!` glyph on a fault, nothing at rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineMark {
    Working,
    Fault,
    None,
}

pub fn machine_mark(state: MachineState) -> MachineMark {
    match state {
        MachineState::Waking => MachineMark::Working,
        MachineState::Error => MachineMark::Fault,
        MachineState::Asleep
        | MachineState::Awake
        | MachineState::Stopped
        | MachineState::Unknown => MachineMark::None,
    }
}

/// The fault glyph at `size` (the red exclamation, the error note's mark).
pub fn fault_glyph(size: f32, theme: &Theme) -> gpui::Svg {
    icon("exclamation", size, theme.danger)
}

/// The machine mark drawn: `None` when the state is at rest, so a corner slot
/// can collapse entirely.
pub fn machine_mark_element(
    state: MachineState,
    glyph_size: f32,
    theme: &Theme,
    cx: &mut App,
) -> Option<AnyElement> {
    match machine_mark(state) {
        MachineMark::Working => Some(working::glyph(theme, cx).into_any_element()),
        MachineMark::Fault => Some(fault_glyph(glyph_size, theme).into_any_element()),
        MachineMark::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_mark_is_spinner_fault_or_nothing() {
        assert_eq!(machine_mark(MachineState::Waking), MachineMark::Working);
        assert_eq!(machine_mark(MachineState::Error), MachineMark::Fault);
        for s in [
            MachineState::Awake,
            MachineState::Asleep,
            MachineState::Stopped,
            MachineState::Unknown,
        ] {
            assert_eq!(machine_mark(s), MachineMark::None, "{s:?} shows no mark");
        }
    }

    #[test]
    fn the_unread_label_caps_at_ninety_nine() {
        assert_eq!(unread_label(1), "1");
        assert_eq!(unread_label(0), "1", "a lit flag with no count reads 1");
        assert_eq!(unread_label(99), "99");
        assert_eq!(unread_label(100), "99+");
    }
}

/// The source lint: no coloured status dot may be drawn in this crate. A
/// "status dot" is a small round `div` (`.size(px(≤ 12))` + `.rounded_full()`)
/// filled with a status colour (`online`, `warn`, `danger`, `accent`, a machine
/// dot colour, or a pulsing alpha of one). Legend swatches and other
/// non-status discs carry `// status-dot: exception <why>` on the `.bg(` line.
#[cfg(test)]
mod lint_tests {
    use std::path::Path;

    const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");

    fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "marks.rs")
            {
                // (this file holds the lint and its sample dot)
                out.push(path);
            }
        }
    }

    fn small_size(line: &str) -> bool {
        let Some(ix) = line.find(".size(px(") else {
            return false;
        };
        let rest = &line[ix + ".size(px(".len()..];
        let num: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        num.parse::<f32>().is_ok_and(|n| n <= 12.0)
    }

    fn status_fill(line: &str) -> bool {
        let Some(ix) = line.find(".bg(") else {
            return false;
        };
        let fill = &line[ix..];
        [
            "theme.online",
            "theme.warn",
            "theme.danger",
            "theme.accent",
            "machine_dot(",
            "theme.unread",
        ]
        .iter()
        .any(|needle| fill.contains(needle))
    }

    pub(super) fn offenders() -> Vec<String> {
        let mut files = Vec::new();
        rust_files(Path::new(SRC), &mut files);
        files.sort();
        let mut found = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).expect("read");
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !line.contains(".rounded_full()") {
                    continue;
                }
                let lo = i.saturating_sub(4);
                let hi = (i + 5).min(lines.len());
                let window = &lines[lo..hi];
                let sized = window.iter().any(|l| small_size(l));
                let filled = window.iter().enumerate().find(|(_, l)| status_fill(l));
                let Some((_, fill_line)) = filled else {
                    continue;
                };
                if !sized {
                    continue;
                }
                if fill_line.contains("status-dot: exception") {
                    continue;
                }
                let rel = file.strip_prefix(SRC).unwrap_or(&file).display();
                found.push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
        found
    }

    #[test]
    fn no_coloured_status_dot_is_drawn_in_this_crate() {
        let found = offenders();
        assert!(
            found.is_empty(),
            "coloured status dots (Jacob 2026-10-27: never; use ui::working / ui::marks, or mark a \
             non-status disc `// status-dot: exception <why>` on its .bg( line):\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn the_lint_catches_a_dot() {
        let sample = [
            "div()",
            "    .size(px(8.0))",
            "    .rounded_full()",
            "    .bg(theme.online)",
        ];
        assert!(super::lint_tests::small_size(sample[1]));
        assert!(super::lint_tests::status_fill(sample[3]));
        assert!(!super::lint_tests::small_size("    .size(px(30.0))"));
        assert!(!super::lint_tests::status_fill("    .bg(theme.surface_2)"));
    }
}
