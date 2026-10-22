//! Colours are paint, numbers drive layout (zeron-study §2.4). One global.
//!
//! The palette is the iPhone app's (`apps/ios/.../UI/Theme.swift`), itself
//! measured from Grok Bot: background #141414, assistant bubble #202020, user
//! bubble #5C5C5C, field #181818 with a #242424 rim, system text #95959B.
//! The Mac is that UI scaled up: a chat-list sidebar, one conversation pane.
//! Matte is the default; Glass is a macOS-only toggle (D10, §3.11).

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{hsla, rgb, rgba, App, Global, Hsla, WindowAppearance, WindowBackgroundAppearance};

use crate::sync::credentials::config_dir;

/// Chat-list sidebar (the iPhone home list, one column).
pub const SIDEBAR_WIDTH: f32 = 296.0;
/// Kept for the settings sub-panels, which open just right of the sidebar.
pub const RAIL_WIDTH: f32 = SIDEBAR_WIDTH;
/// The conversation header (the centred title pill) — also the window-drag
/// catchment at the top of the chat pane. Grok Mac, measured on the real app
/// (ref 57, 2 px/pt): the hairline sits at 44 pt and the title row is centred
/// at 22 pt, on the traffic lights' line. The doubling to 88 (#1226) made the
/// header itself too big ([feedback mac-header-height-grok]).
pub const HEADER_HEIGHT: f32 = 44.0;
pub const TAB_BAR_HEIGHT: f32 = HEADER_HEIGHT;
/// Composer: one pill with the "+" circle inside left and mic / send inside right.
pub const COMPOSER_WIDTH: f32 = 820.0;
/// Grok Mac, measured on the real app (refs 57/58, 2 px/pt): a 44 pt pill with
/// 28 pt circles inset 8, 16 pt side margins and 16 pt below; bubbles inset 21.
pub const COMPOSER_HEIGHT: f32 = 44.0;
pub const COMPOSER_MARGIN: f32 = 16.0;
pub const COMPOSER_BOTTOM: f32 = 16.0;
pub const COLUMN_MARGIN: f32 = 21.0;
pub const PLUS_BUTTON: f32 = 28.0;
pub const SEND_BUTTON: f32 = 28.0;
/// Icon buttons in the header and sheet corners.
pub const CONTROL: f32 = 32.0;
/// Bubble corners (Grok Mac, measured: 12 pt; the iPhone uses 20).
pub const BUBBLE_RADIUS: f32 = 12.0;
/// Chat bubbles: Grok Mac's body is 13.5 pt on a 20 pt pitch with 15 pt
/// horizontal and 6 pt vertical padding (refs 57/58); the rest of the app
/// keeps `TEXT_BODY`.
pub const CHAT_BODY: f32 = 13.5;
pub const CHAT_LINE_HEIGHT: f32 = 20.0;
pub const BUBBLE_PAD_X: f32 = 15.0;
pub const BUBBLE_PAD_Y: f32 = 6.0;
pub const PANEL_RADIUS: f32 = 14.0;
pub const DRAWER_WIDTH: f32 = 320.0;
pub const NARROW_WINDOW: f32 = 1000.0;
pub const GLASS_ALPHA: f32 = 0.80;
/// Light + Glass tints the blur with pure white; a touch more opaque than the
/// dark wash so the chat reads as a white surface, not grey over the desktop.
pub const GLASS_ALPHA_LIGHT: f32 = 0.86;
pub const FROST_TINT_ALPHA: f32 = 0.50;
pub const FROST_TINT_ALPHA_LIGHT: f32 = 0.72;
/// Glass shell tint: how opaque the window wash is over the blur (Settings →
/// Appearance → Opacity). Default is [`GLASS_ALPHA`] / [`GLASS_ALPHA_LIGHT`];
/// range keeps text readable at the low end and still looks like glass at
/// the high end.
pub const GLASS_OPACITY_MIN: f32 = 0.30;
pub const GLASS_OPACITY_MAX: f32 = 1.00;
const GLASS_OPACITY_FILE: &str = "glass_opacity";
/// Type scale (iPhone 17/15/13 pt, scaled for a desktop viewing distance).
pub const TEXT_BODY: f32 = 15.0;
pub const TEXT_SMALL: f32 = 13.0;
pub const TEXT_TINY: f32 = 11.5;
pub const LINE_HEIGHT: f32 = 21.0;
/// Avatar sizes: list row (Grok Mac: 33 pt blobs in a 58 pt row), header, inline sender.
pub const AVATAR_ROW: f32 = 36.0;
pub const AVATAR_HEADER: f32 = 22.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Matte,
    Glass,
}

const SURFACE_FILE: &str = "surface";

impl Surface {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "matte" => Some(Self::Matte),
            "glass" => Some(Self::Glass),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Matte => "matte",
            Self::Glass => "glass",
        }
    }

    /// The saved choice, or Matte (D10). Saved as one word in `<config>/surface`
    /// only when picked in Settings, like the appearance.
    pub fn load() -> Self {
        std::fs::read_to_string(config_dir().join(SURFACE_FILE))
            .ok()
            .and_then(|s| Self::parse(&s))
            .unwrap_or(Self::Matte)
    }

    pub fn save(self) {
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(SURFACE_FILE), self.as_str());
    }
}

/// The user's glass opacity (0.30–1.00). Saved as a float string in
/// `<config>/glass_opacity` when moved in Settings; a missing file keeps the
/// default for the appearance ([`GLASS_ALPHA`] dark, [`GLASS_ALPHA_LIGHT`]
/// light). Only applied while Surface is Glass.
pub struct GlassOpacity;

impl GlassOpacity {
    pub fn clamp(v: f32) -> f32 {
        v.clamp(GLASS_OPACITY_MIN, GLASS_OPACITY_MAX)
    }

    /// The default wash for an appearance when nothing is saved.
    pub fn default_for(appearance: Appearance) -> f32 {
        match appearance {
            Appearance::Dark => GLASS_ALPHA,
            Appearance::Light => GLASS_ALPHA_LIGHT,
        }
    }

    /// The saved choice, or the appearance's default.
    pub fn load(appearance: Appearance) -> f32 {
        std::fs::read_to_string(config_dir().join(GLASS_OPACITY_FILE))
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .map(Self::clamp)
            .unwrap_or_else(|| Self::default_for(appearance))
    }

    pub fn save(v: f32) {
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join(GLASS_OPACITY_FILE),
            format!("{:.2}", Self::clamp(v)),
        );
    }
}

/// What the window is drawn in right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

/// What the user chose (D81): System follows macOS and is the default; the
/// order is the Settings order. Saved as one word in `<config>/appearance`
/// only when the user picks in Settings, so a missing file means "never
/// chose" → System, and an explicit `dark` or `light` stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppearancePref {
    System,
    Light,
    Dark,
}

const APPEARANCE_FILE: &str = "appearance";

/// macOS's own light/dark, mirrored from the last window-appearance reading so
/// code without a `cx` (notification images) can follow it.
static SYSTEM_DARK: AtomicBool = AtomicBool::new(false);

impl AppearancePref {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The saved choice, or System.
    pub fn load() -> Self {
        std::fs::read_to_string(config_dir().join(APPEARANCE_FILE))
            .ok()
            .and_then(|s| Self::parse(&s))
            .unwrap_or(Self::System)
    }

    pub fn save(self) {
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(APPEARANCE_FILE), self.as_str());
    }

    /// The appearance to draw: System takes macOS's; anything unknown is Light.
    pub fn resolve(self, system: WindowAppearance) -> Appearance {
        match self {
            Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
            Self::System => match system {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            },
        }
    }
}

/// Remember macOS's current light/dark (startup and every appearance change).
pub fn note_system_appearance(system: WindowAppearance) {
    SYSTEM_DARK.store(
        matches!(
            system,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
        Ordering::Relaxed,
    );
}

/// Is macOS itself dark right now? Notification banners are drawn by the
/// system, so their pictures follow this rather than the in-app theme (D81).
pub fn system_is_dark() -> bool {
    SYSTEM_DARK.load(Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub surface: Surface,
    pub appearance: Appearance,
    /// The user's choice behind `appearance` (System / Light / Dark).
    pub pref: AppearancePref,
    /// Shell tint alpha while Glass is on ([`GlassOpacity`]; default [`GLASS_ALPHA`]).
    pub glass_opacity: f32,
    pub bg: Hsla,
    /// The chat-list column (a shade off the conversation, as on Grok Mac).
    pub sidebar: Hsla,
    /// The right pane (details, members, settings — the three-pane rule) and
    /// the grouped cards on it. Light: a grey pane with white cards, Apple's
    /// Account-screen grouping (D80); dark: the card tones the pane had before.
    pub pane: Hsla,
    pub pane_card: Hsla,
    /// Cards, sheets, rows that lift off the background (iPhone `card`).
    pub surface_1: Hsla,
    /// Hover / selected fills and chips (the assistant-bubble grey).
    pub surface_2: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    /// System lines, timestamps, previews, "is working" (#95959B).
    pub text_muted: Hsla,
    /// Placeholders and footnotes.
    pub text_faint: Hsla,
    /// Grok's blue: unread dots, links, the caret.
    pub accent: Hsla,
    pub accent_soft: Hsla,
    /// User bubble (right).
    pub bubble: Hsla,
    /// Assistant bubble (left).
    pub bubble_assistant: Hsla,
    /// Composer field / glass circle fill and its hairline rim.
    pub field: Hsla,
    pub field_border: Hsla,
    /// Primary filled control (send / voice button): white on dark.
    pub primary: Hsla,
    pub on_primary: Hsla,
    /// Presence green.
    pub online: Hsla,
    pub warn: Hsla,
    pub danger: Hsla,
    pub danger_soft: Hsla,
    pub code_bg: Hsla,
    pub font_sans: &'static str,
    pub font_mono: &'static str,
}

impl Global for Theme {}

fn grey(l: f32) -> Hsla {
    hsla(0.0, 0.0, l, 1.0)
}

fn hex(v: u32) -> Hsla {
    rgb(v).into()
}

fn hex_a(v: u32, a: f32) -> Hsla {
    rgba((v << 8) | ((a * 255.0).round() as u32 & 0xff)).into()
}

impl Theme {
    pub fn dark(surface: Surface) -> Self {
        Self {
            surface,
            appearance: Appearance::Dark,
            pref: AppearancePref::Dark,
            glass_opacity: GLASS_ALPHA,
            bg: hex(0x141414),
            sidebar: hex(0x1a1a1a),
            pane: hex(0x1e1e1e),
            pane_card: hex(0x232323),
            surface_1: hex(0x1e1e1e),
            surface_2: hex(0x262626),
            border: hex_a(0xffffff, 0.08),
            border_strong: hex_a(0xffffff, 0.16),
            text: grey(1.0),
            text_muted: hex(0x95959b),
            text_faint: hex(0x6a6a6e),
            accent: hex(0x1084fe),
            accent_soft: hex_a(0x1084fe, 0.18),
            bubble: hex(0x5c5c5c),
            bubble_assistant: hex(0x232323),
            field: hex(0x181818),
            field_border: hex(0x242424),
            primary: grey(1.0),
            on_primary: grey(0.0),
            online: hex(0x34c759),
            warn: hex(0xffb340),
            danger: hex(0xff453a),
            danger_soft: hex_a(0xff453a, 0.14),
            code_bg: hex_a(0x000000, 0.28),
            font_sans: Self::sans_family(),
            font_mono: Self::mono_family(),
        }
    }

    pub fn light(surface: Surface) -> Self {
        Self {
            surface,
            appearance: Appearance::Light,
            pref: AppearancePref::Light,
            glass_opacity: GLASS_ALPHA_LIGHT,
            // Grok Mac light (ref 16/30): chat #FCFCFC, assistant bubble
            // #F2F2F2, white field with a hairline. The flanks step away
            // from the chat more than Grok's #F6F6F6 so the three panes read
            // at a glance (Jacob, 2026-10-21): sidebar #F1F1F1 | chat #FCFCFC
            // | pane #F5F5F5 with white cards. Greys meet WCAG AA on every
            // surface they sit on: muted 5.3:1 on white / 4.7:1 on a bubble,
            // faint 4.6:1 on white, accent 4.8:1 on white (#1084FE was 3.7:1).
            bg: hex(0xfcfcfc),
            sidebar: hex(0xf1f1f1),
            pane: hex(0xf5f5f5),
            pane_card: grey(1.0),
            surface_1: grey(1.0),
            surface_2: hex(0xe8e8e8),
            border: hex_a(0x000000, 0.10),
            border_strong: hex_a(0x000000, 0.18),
            text: hex(0x111111),
            text_muted: hex(0x6b6b70),
            text_faint: hex(0x757579),
            accent: hex(0x0a6fe0),
            accent_soft: hex_a(0x0a6fe0, 0.14),
            bubble: hex(0x111111),
            bubble_assistant: hex(0xf2f2f2),
            field: grey(1.0),
            field_border: hex(0xdedede),
            primary: hex(0x111111),
            on_primary: grey(1.0),
            online: hex(0x34c759),
            warn: hex(0xe59a00),
            danger: hex(0xd7332a),
            danger_soft: hex_a(0xd7332a, 0.10),
            code_bg: hex_a(0x000000, 0.06),
            font_sans: Self::sans_family(),
            font_mono: Self::mono_family(),
        }
    }

    pub fn of(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    pub fn with(appearance: Appearance, surface: Surface) -> Self {
        match appearance {
            Appearance::Dark => Self::dark(surface),
            Appearance::Light => Self::light(surface),
        }
    }

    /// The theme for a choice: System resolves against macOS's current appearance.
    pub fn for_pref(pref: AppearancePref, surface: Surface, cx: &App) -> Self {
        let system = cx.window_appearance();
        note_system_appearance(system);
        let mut theme = Self::with(pref.resolve(system), surface);
        theme.pref = pref;
        theme.glass_opacity = GlassOpacity::load(theme.appearance);
        theme
    }

    fn sans_family() -> &'static str {
        if cfg!(target_os = "macos") {
            ".SystemUIFont"
        } else {
            "Inter"
        }
    }

    fn mono_family() -> &'static str {
        if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "JetBrains Mono"
        }
    }

    /// Text on a user bubble: white on the dark grey, white on black.
    pub fn bubble_text(&self) -> Hsla {
        grey(1.0)
    }

    /// Glass is only possible where the window can be blurred (D10).
    pub fn glass_available() -> bool {
        cfg!(target_os = "macos")
    }

    /// Window vibrancy: Glass selected and the platform can blur the window.
    pub fn is_glass(&self) -> bool {
        self.surface == Surface::Glass && Self::glass_available()
    }

    /// In-window frosted panels: Glass selected. Works on every backend the
    /// zui fork supports (Metal, wgpu, DirectX), so Linux previews it too.
    pub fn is_frost(&self) -> bool {
        self.surface == Surface::Glass
    }

    fn is_light(&self) -> bool {
        self.appearance == Appearance::Light
    }

    /// Per-appearance alpha for a translucent layer: light glass needs more
    /// tint than dark to stay white over a dark desktop.
    fn frost_alpha(&self, dark: f32, light: f32) -> f32 {
        if self.is_light() {
            light
        } else {
            dark
        }
    }

    /// The shell tint painted on the root (zeron-study §3.1 step 2). Light
    /// glass tints with pure white, not the off-white page colour: the blur
    /// underneath is already grey, and a grey-on-grey wash is what made Light
    /// + Glass look muddy (Jacob's shot, 2026-10-21).
    pub fn shell(&self) -> Hsla {
        if !self.is_glass() {
            return self.bg;
        }
        let mut c = if self.is_light() { grey(1.0) } else { self.bg };
        c.a = GlassOpacity::clamp(self.glass_opacity);
        c
    }

    /// Floating cards: translucent tint on frost, flat card on matte (§3.11).
    pub fn card(&self) -> Hsla {
        let mut c = self.surface_1;
        c.a = if self.is_frost() {
            self.frost_alpha(FROST_TINT_ALPHA, FROST_TINT_ALPHA_LIGHT)
        } else {
            1.0
        };
        c
    }

    /// The right pane (three-pane rule): its own tone, so it reads apart from
    /// the chat beside it; translucent like a card on frost.
    pub fn pane(&self) -> Hsla {
        let mut c = self.pane;
        c.a = if self.is_frost() {
            self.frost_alpha(FROST_TINT_ALPHA, FROST_TINT_ALPHA_LIGHT)
        } else {
            1.0
        };
        c
    }

    /// Chrome strips (sidebar, side panels) over frost: thinner than cards.
    pub fn chrome(&self) -> Hsla {
        let mut c = self.sidebar;
        c.a = if self.is_frost() {
            self.frost_alpha(0.35, 0.60)
        } else {
            1.0
        };
        c
    }

    /// The glass circle / pill fill of the iPhone header controls: the field
    /// colour at 85 % over the (frosted) background.
    pub fn glass_fill(&self) -> Hsla {
        let mut c = self.field;
        c.a = 0.85;
        c
    }

    pub fn window_background_appearance(&self) -> WindowBackgroundAppearance {
        if self.is_glass() {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        }
    }

    pub fn with_alpha(mut c: Hsla, a: f32) -> Hsla {
        c.a = a;
        c
    }
}

/// GPUI's macOS backend drops the blur view when the appearance changes
/// (zeron-study §3.1 step 4). Re-apply after every theme change, deferred.
pub fn reapply_window_background(cx: &mut App) {
    cx.defer(|cx| {
        let wanted = Theme::of(cx).window_background_appearance();
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, _| {
                window.set_background_appearance(wanted);
            });
        }
    });
}
