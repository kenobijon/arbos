//! Colours are paint, numbers drive layout (zeron-study §2.4). One global.
//! Matte is the default; Glass is a macOS-only toggle (D10, §3.11).

use gpui::{hsla, App, Global, Hsla, WindowBackgroundAppearance};

pub const RAIL_WIDTH: f32 = 90.0;
pub const RAIL_WIDTH_COLLAPSED: f32 = 48.0;
pub const RAIL_TILE: f32 = 72.0;
pub const TAB_BAR_HEIGHT: f32 = 38.0;
pub const TAB_MIN_WIDTH: f32 = 120.0;
pub const COMPOSER_WIDTH: f32 = 640.0;
pub const COMPOSER_HEIGHT: f32 = 48.0;
pub const BUBBLE_RADIUS: f32 = 14.0;
pub const PANEL_RADIUS: f32 = 10.0;
pub const RIGHT_PANEL_WIDTH: f32 = 360.0;
pub const DRAWER_WIDTH: f32 = 320.0;
pub const NARROW_WINDOW: f32 = 1100.0;
pub const GLASS_ALPHA: f32 = 0.80;
pub const FROST_TINT_ALPHA: f32 = 0.50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Matte,
    Glass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub surface: Surface,
    pub appearance: Appearance,
    pub bg: Hsla,
    pub rail_wash: Hsla,
    pub surface_1: Hsla,
    pub surface_2: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub bubble: Hsla,
    pub ok: Hsla,
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

impl Theme {
    pub fn dark(surface: Surface) -> Self {
        Self {
            surface,
            appearance: Appearance::Dark,
            bg: hsla(0.0, 0.0, 0.17, 1.0),
            rail_wash: hsla(0.0, 0.0, 0.0, 0.10),
            surface_1: hsla(0.0, 0.0, 0.22, 1.0),
            surface_2: hsla(0.0, 0.0, 0.27, 1.0),
            border: hsla(0.0, 0.0, 1.0, 0.09),
            border_strong: hsla(0.0, 0.0, 1.0, 0.18),
            text: grey(0.92),
            text_muted: grey(0.64),
            text_faint: grey(0.46),
            accent: hsla(0.70, 0.82, 0.74, 1.0),
            accent_soft: hsla(0.70, 0.82, 0.74, 0.18),
            bubble: hsla(0.0, 0.0, 0.26, 0.9),
            ok: hsla(0.40, 0.55, 0.55, 1.0),
            warn: hsla(0.12, 0.85, 0.62, 1.0),
            danger: hsla(0.0, 0.70, 0.62, 1.0),
            danger_soft: hsla(0.0, 0.70, 0.62, 0.14),
            code_bg: hsla(0.0, 0.0, 0.0, 0.25),
            font_sans: Self::sans_family(),
            font_mono: Self::mono_family(),
        }
    }

    pub fn light(surface: Surface) -> Self {
        Self {
            surface,
            appearance: Appearance::Light,
            bg: hsla(0.0, 0.0, 0.965, 1.0),
            rail_wash: hsla(0.0, 0.0, 0.0, 0.04),
            surface_1: hsla(0.0, 0.0, 0.93, 1.0),
            surface_2: hsla(0.0, 0.0, 0.88, 1.0),
            border: hsla(0.0, 0.0, 0.0, 0.08),
            border_strong: hsla(0.0, 0.0, 0.0, 0.16),
            text: grey(0.12),
            text_muted: grey(0.42),
            text_faint: grey(0.58),
            accent: hsla(0.70, 0.70, 0.52, 1.0),
            accent_soft: hsla(0.70, 0.70, 0.52, 0.16),
            bubble: hsla(0.0, 0.0, 0.88, 0.9),
            ok: hsla(0.40, 0.55, 0.40, 1.0),
            warn: hsla(0.10, 0.85, 0.48, 1.0),
            danger: hsla(0.0, 0.65, 0.50, 1.0),
            danger_soft: hsla(0.0, 0.65, 0.50, 0.12),
            code_bg: hsla(0.0, 0.0, 0.0, 0.06),
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

    /// Glass is only possible where the window can be blurred (D10).
    pub fn glass_available() -> bool {
        cfg!(target_os = "macos")
    }

    pub fn is_glass(&self) -> bool {
        self.surface == Surface::Glass && Self::glass_available()
    }

    /// The shell tint painted on the root (zeron-study §3.1 step 2).
    pub fn shell(&self) -> Hsla {
        let mut c = self.bg;
        c.a = if self.is_glass() { GLASS_ALPHA } else { 1.0 };
        c
    }

    /// Floating cards: translucent tint on glass, flat card on matte (§3.11).
    pub fn card(&self) -> Hsla {
        let mut c = self.surface_1;
        c.a = if self.is_glass() {
            FROST_TINT_ALPHA
        } else {
            1.0
        };
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
