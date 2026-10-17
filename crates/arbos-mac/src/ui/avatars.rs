//! Telegram-style initials avatars (D79): a disc with one of seven vertical
//! gradients, picked from the stable id (agents: project id; people: user id)
//! unless the agent's owner chose one (`avatar_color_index`), with white
//! semibold initials (one or two letters) at 44.5 % of the diameter. A photo
//! (GitHub avatar) always wins over initials — see [`crate::ui::people::avatar`].
//! The brand mark is the only image left in the app; no character art.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, Div, FontWeight, Hsla};

use crate::model::{AgentMember, Person, Project, Sender};
use crate::ui::theme::{Appearance, Theme};

/// What the agent is doing (from its status). Kept for callers that pulse
/// while an agent works; the disc itself does not animate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AvatarState {
    Idle,
    Working,
    Reply,
}

/// One Telegram gradient: light and dark (HSL lightness −7 %, the relay's
/// `GET /avatar-colors` table) top/bottom pairs, and its name for the picker.
#[derive(Clone, Copy, Debug)]
pub struct Gradient {
    pub name: &'static str,
    pub top: [u8; 3],
    pub bottom: [u8; 3],
    pub dark_top: [u8; 3],
    pub dark_bottom: [u8; 3],
}

/// The seven Telegram peer colours, in Telegram's order (indices 0–6).
pub const GRADIENTS: [Gradient; 7] = [
    Gradient {
        name: "Red",
        top: [0xFF, 0x84, 0x5E],
        bottom: [0xD4, 0x52, 0x46],
        dark_top: [0xFF, 0x69, 0x3A],
        dark_bottom: [0xC8, 0x3B, 0x2E],
    },
    Gradient {
        name: "Orange",
        top: [0xFE, 0xBB, 0x5B],
        bottom: [0xF6, 0x81, 0x36],
        dark_top: [0xFE, 0xAC, 0x38],
        dark_bottom: [0xF4, 0x6C, 0x14],
    },
    Gradient {
        name: "Violet",
        top: [0xB6, 0x94, 0xF9],
        bottom: [0x6C, 0x61, 0xDF],
        dark_top: [0x9F, 0x72, 0xF7],
        dark_bottom: [0x50, 0x43, 0xD9],
    },
    Gradient {
        name: "Green",
        top: [0x9A, 0xD1, 0x64],
        bottom: [0x46, 0xBA, 0x43],
        dark_top: [0x88, 0xC9, 0x48],
        dark_bottom: [0x3C, 0xA0, 0x3A],
    },
    Gradient {
        name: "Cyan",
        top: [0x53, 0xED, 0xD6],
        bottom: [0x28, 0xC9, 0xB7],
        dark_top: [0x33, 0xEA, 0xCE],
        dark_bottom: [0x22, 0xAB, 0x9C],
    },
    Gradient {
        name: "Blue",
        top: [0x5C, 0xAF, 0xFA],
        bottom: [0x40, 0x8A, 0xCF],
        dark_top: [0x39, 0x9E, 0xF9],
        dark_bottom: [0x2F, 0x78, 0xBC],
    },
    Gradient {
        name: "Pink",
        top: [0xFF, 0x8A, 0xA5],
        bottom: [0xE0, 0x5F, 0x8E],
        dark_top: [0xFF, 0x66, 0x8A],
        dark_bottom: [0xDA, 0x41, 0x79],
    },
];

/// Initials size as a fraction of the disc (Telegram: SF Semibold at 44.5 %,
/// one and two letters alike); the iPhone shares this constant.
pub const INITIALS_SCALE: f32 = 0.445;

/// `(sum of the id's UTF-8 bytes) mod 7` — the same rule on every client.
pub fn color_index(seed: &str) -> u8 {
    (seed.bytes().map(u32::from).sum::<u32>() % 7) as u8
}

/// The gradient for a seed, or the owner's override when there is one.
pub fn index_for(seed: &str, override_index: Option<u8>) -> u8 {
    match override_index {
        Some(i) if i < 7 => i,
        _ => color_index(seed),
    }
}

fn rgb(c: [u8; 3]) -> Hsla {
    gpui::Rgba {
        r: c[0] as f32 / 255.0,
        g: c[1] as f32 / 255.0,
        b: c[2] as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Top and bottom colours of gradient `index` for the current appearance.
pub fn gradient(index: u8, theme: &Theme) -> (Hsla, Hsla) {
    let g = GRADIENTS[(index as usize) % 7];
    match theme.appearance {
        Appearance::Light => (rgb(g.top), rgb(g.bottom)),
        Appearance::Dark => (rgb(g.dark_top), rgb(g.dark_bottom)),
    }
}

/// One or two letters: the first letter of the first two words ("Growth Lab"
/// → "GL"), one for a single word ("Arbos" → "A"). Leading `@` and
/// punctuation are skipped; an empty name gives "?".
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '·' || c == '/')
        .map(|w| w.trim_start_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    let first = |w: &str| {
        w.chars()
            .next()
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_default()
    };
    match words.as_slice() {
        [] => "?".into(),
        [one] => first(one),
        [a, b, ..] => format!("{}{}", first(a), first(b)),
    }
}

/// The avatar disc: gradient `index` (or the seed's) with the name's initials.
pub fn element(seed: &str, name: &str, index: Option<u8>, size: f32, theme: &Theme) -> Div {
    let (top, bottom) = gradient(index_for(seed, index), theme);
    let text = initials(name);
    let font = size * INITIALS_SCALE;
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(linear_gradient(
            180.0,
            linear_color_stop(top, 0.0),
            linear_color_stop(bottom, 1.0),
        ))
        .text_size(px(font))
        // A tight line box: with the 1.1× box GPUI centred, SF's ascent pushed
        // the caps ~0.75 pt below the disc centre (parity pass, Telegram ≈ 0.17 pt).
        .line_height(px(font))
        .pb(px(size * 0.02))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(gpui::white())
        .child(text)
}

/// An agent (project): its id seeds the colour, the owner's pick overrides.
pub fn project(p: &Project, size: f32, theme: &Theme) -> Div {
    element(&p.id, &p.name, p.avatar_color_index, size, theme)
}

/// An agent shared into a people chat.
pub fn agent_member(a: &AgentMember, size: f32, theme: &Theme) -> Div {
    element(&a.project_id, &a.name, a.avatar_color_index, size, theme)
}

/// Who started a call / sits in it (users and agents).
pub fn sender(s: &Sender, size: f32, theme: &Theme) -> Div {
    element(&s.id, &s.display_name(), s.avatar_color_index, size, theme)
}

/// A person without a photo.
pub fn person(p: &Person, size: f32, theme: &Theme) -> Div {
    let seed = if p.id.is_empty() { &p.handle } else { &p.id };
    element(seed, &p.display_name(), None, size, theme)
}

/// A picker swatch: the plain gradient disc, no letters.
pub fn swatch(index: u8, size: f32, theme: &Theme) -> Div {
    let (top, bottom) = gradient(index, theme);
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(linear_gradient(
            180.0,
            linear_color_stop(top, 0.0),
            linear_color_stop(bottom, 1.0),
        ))
}

// ----- notification attachment --------------------------------------------------

fn fonts() -> &'static Mutex<(cosmic_text::FontSystem, cosmic_text::SwashCache)> {
    static FONTS: OnceLock<Mutex<(cosmic_text::FontSystem, cosmic_text::SwashCache)>> =
        OnceLock::new();
    FONTS.get_or_init(|| {
        Mutex::new((
            cosmic_text::FontSystem::new(),
            cosmic_text::SwashCache::new(),
        ))
    })
}

/// The same disc as a 128 px PNG for notification banners (cached by
/// gradient + initials under the notify image dir). `dark` picks the dark
/// pair: the system draws the banner, so callers pass macOS's own appearance
/// (`theme::system_is_dark()`), not the in-app theme (D81).
pub fn notification_png(seed: &str, name: &str, index: Option<u8>, dark: bool) -> Option<PathBuf> {
    let idx = index_for(seed, index);
    let text = initials(name);
    let variant = if dark { "-dark" } else { "" };
    let key = format!("initials-{idx}-{}{variant}", text.to_lowercase());
    let size = 128u32;
    let g = GRADIENTS[idx as usize];
    let (top, bottom) = if dark {
        (g.dark_top, g.dark_bottom)
    } else {
        (g.top, g.bottom)
    };
    let mut out = image::RgbaImage::new(size, size);
    let c = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 / 2.0;
    for (x, y, px) in out.enumerate_pixels_mut() {
        let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
        let cover = (radius - d + 0.5).clamp(0.0, 1.0);
        if cover <= 0.0 {
            continue;
        }
        let t = y as f32 / (size as f32 - 1.0);
        let mix = |i: usize| (top[i] as f32 * (1.0 - t) + bottom[i] as f32 * t) as u8;
        *px = image::Rgba([mix(0), mix(1), mix(2), (cover * 255.0) as u8]);
    }
    draw_initials(&mut out, &text);
    crate::ui::notify::image_file(&key, size, size, out.as_raw())
}

/// White semibold initials centred on the disc, through cosmic-text and the
/// system's sans-serif.
fn draw_initials(out: &mut image::RgbaImage, text: &str) {
    use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, Weight};
    let Ok(mut guard) = fonts().lock() else {
        return;
    };
    let (font_system, cache) = &mut *guard;
    let size = out.width() as f32;
    let font_px = size * INITIALS_SCALE;
    let metrics = Metrics::new(font_px, font_px * 1.2);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(Some(size), Some(size));
    buffer.set_text(
        text,
        &Attrs::new()
            .family(Family::SansSerif)
            .weight(Weight::SEMIBOLD),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, false);
    // Centre: measure the laid-out run, then offset while drawing.
    let (mut width, mut ascent, mut descent) = (0.0f32, 0.0f32, 0.0f32);
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
        ascent = ascent.max(run.line_y - run.line_top);
        descent = descent.max(run.line_height - (run.line_y - run.line_top));
    }
    let text_h = ascent + descent;
    let dx = ((size - width) / 2.0).round() as i32;
    let dy = ((size - text_h) / 2.0).round() as i32;
    let white = cosmic_text::Color::rgb(255, 255, 255);
    let (w, h) = (out.width() as i32, out.height() as i32);
    buffer.draw(font_system, cache, white, |x, y, bw, bh, color| {
        let a = color.a() as f32 / 255.0;
        if a <= 0.0 {
            return;
        }
        for yy in y..y + bh as i32 {
            for xx in x..x + bw as i32 {
                let (px_x, px_y) = (xx + dx, yy + dy);
                if px_x < 0 || px_y < 0 || px_x >= w || px_y >= h {
                    continue;
                }
                let p = out.get_pixel_mut(px_x as u32, px_y as u32);
                let blend = |c: u8| (c as f32 * (1.0 - a) + 255.0 * a) as u8;
                *p = image::Rgba([
                    blend(p[0]),
                    blend(p[1]),
                    blend(p[2]),
                    p[3].max((a * 255.0) as u8),
                ]);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_follow_the_rule() {
        assert_eq!(initials("Growth Lab"), "GL");
        assert_eq!(initials("Arbos"), "A");
        assert_eq!(initials("@alpha-lab"), "AL");
        assert_eq!(initials("sam rivera jones"), "SR");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn colour_index_is_byte_sum_mod_seven() {
        assert_eq!(color_index(""), 0);
        assert_eq!(color_index("a"), (97 % 7) as u8);
        // Reference values from the relay's spec.
        assert_eq!(color_index("gh_1"), 1);
        assert_eq!(color_index("u_mac"), 6);
        assert_eq!(
            color_index("01M50W7HPM0KCFVMQ32HXNTN5S"),
            color_index("01M50W7HPM0KCFVMQ32HXNTN5S")
        );
        assert!(color_index("01M50W7HPM0KCFVMQ32HXNTN5S") < 7);
        assert_eq!(index_for("x", Some(3)), 3);
        assert_eq!(index_for("x", Some(9)), color_index("x"));
    }
}
