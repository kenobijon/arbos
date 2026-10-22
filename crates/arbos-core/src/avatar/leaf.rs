//! Leaf avatars (D92): the default picture of an agent that has no uploaded
//! photo. A cream leaf on a coloured disc, one of nine shapes × ten autumn
//! colours. People keep their D79 initials.
//!
//! * The shapes are Jacob's single-path SVGs in `assets/avatars/leaves/svg/`
//!   (`manifest.json` beside them is the source of truth for ids, names,
//!   stem pivots and the palette; `lottie/` holds the idle loop and the
//!   hover gust per leaf). Swapping the art is a file replace: the files are
//!   compiled in here, so the Mac, the relay and — through arbos-ffi — the
//!   iPhone draw the same outline.
//! * The default pick is deterministic from the agent id, as the manifest
//!   README asks: `h = fnv1a64(id)`, `leaf = h mod 9`, `colour = (h div 9)
//!   mod 10`. A byte sum (the D79 rule) clusters neighbouring ids; the hash
//!   spreads them over the 90 cells.
//! * The owner may pin a leaf and a colour (`avatar_leaf`, `avatar_leaf_color`
//!   on the relay, `null` = automatic).
//! * [`segments`] gives the outline as absolute move / line / cubic commands,
//!   for clients that draw the path natively instead of rendering the SVG.

use std::sync::OnceLock;

use super::{Accent, Rgb};

/// Number of leaf shapes.
pub const LEAF_COUNT: u8 = 9;
/// Number of disc colours.
pub const COLOR_COUNT: u8 = 10;
/// The leaf's ink on every disc colour.
pub const LEAF_INK: Rgb = Rgb(0xF6, 0xF0, 0xE8);
/// The leaf's span as a share of the disc diameter (the SVG box is 100 × 100).
pub const LEAF_SCALE: f32 = 0.62;
/// Bump when the index rule changes, so clients can refresh cached defaults.
pub const LEAF_RULE_VERSION: u32 = 1;

/// One leaf shape: its asset id (`leaf-NN-name`), picker label, SVG source,
/// stem pivot (the point the leaf sways about, in the 100 × 100 box) and the
/// idle loop length in seconds (staggered per leaf so a list never sways in
/// unison; 60 fps × `idle_frames` from the manifest).
#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub slug: &'static str,
    pub name: &'static str,
    pub svg: &'static str,
    /// Jacob's Lottie files (bodymovin 5.7, 100 × 100, 60 fps): the seamless idle loop and the
    /// one-shot 1 s gust. One shape layer `leaf` → group → fill `leaf`; recolour via keypath
    /// `**.leaf.Color`; transparent background (the app draws the disc).
    pub idle_lottie: &'static str,
    pub gust_lottie: &'static str,
    pub pivot: (f32, f32),
    pub idle_secs: f32,
    /// The leaf's own default disc colour from the manifest (index into [`COLORS`]).
    pub default_color: u8,
}

macro_rules! leaf {
    ($slug:literal, $name:literal, $pivot:expr, $frames:literal, $color:literal) => {
        Leaf {
            slug: $slug,
            name: $name,
            svg: include_str!(concat!(
                "../../../../assets/avatars/leaves/svg/",
                $slug,
                ".svg"
            )),
            idle_lottie: include_str!(concat!(
                "../../../../assets/avatars/leaves/lottie/",
                $slug,
                "-idle.json"
            )),
            gust_lottie: include_str!(concat!(
                "../../../../assets/avatars/leaves/lottie/",
                $slug,
                "-gust.json"
            )),
            pivot: $pivot,
            idle_secs: $frames as f32 / 60.0,
            default_color: $color,
        }
    };
}

/// `assets/avatars/leaves/manifest.json`, compiled in so a test can check this table against it.
pub const MANIFEST_JSON: &str = include_str!("../../../../assets/avatars/leaves/manifest.json");

/// The nine shapes, in the manifest's order (indices 0–8 = ids 1–9).
pub const LEAVES: [Leaf; LEAF_COUNT as usize] = [
    leaf!("leaf-01-maple", "Maple", (48.626, 80.443), 150, 9),
    leaf!("leaf-02-teardrop", "Teardrop", (32.663, 80.985), 160, 3),
    leaf!("leaf-03-poplar", "Poplar", (47.492, 80.993), 140, 0),
    leaf!("leaf-04-split", "Split", (39.268, 81.005), 170, 4),
    leaf!("leaf-05-cloud-tree", "Cloud tree", (49.156, 81.0), 150, 1),
    leaf!("leaf-06-feather", "Feather", (42.027, 80.963), 144, 6),
    leaf!("leaf-07-curl", "Curl", (42.141, 80.961), 156, 5),
    leaf!("leaf-08-sweetgum", "Sweetgum", (49.914, 79.021), 166, 7),
    leaf!("leaf-09-grass", "Grass", (48.196, 80.974), 136, 2),
];

/// The hover gust: one shot, about this long, settling back to the rest pose.
pub const GUST_SECS: f32 = 1.0;
/// Peak gust angle, degrees (the Lottie swings to about 9°).
pub const GUST_DEGREES: f32 = 9.0;
/// Peak idle sway angle, degrees ("a few degrees about the stem base").
pub const IDLE_DEGREES: f32 = 3.0;

/// One disc colour of the autumn palette.
#[derive(Clone, Copy, Debug)]
pub struct LeafColor {
    pub name: &'static str,
    pub rgb: Rgb,
}

/// The ten disc colours, in Jacob's order (indices 0–9).
pub const COLORS: [LeafColor; COLOR_COUNT as usize] = [
    LeafColor {
        name: "Mustard",
        rgb: Rgb(0xBC, 0xA3, 0x68),
    },
    LeafColor {
        name: "Olive",
        rgb: Rgb(0x8D, 0x96, 0x59),
    },
    LeafColor {
        name: "Moss",
        rgb: Rgb(0x60, 0x64, 0x40),
    },
    LeafColor {
        name: "Sage",
        rgb: Rgb(0x60, 0x8F, 0x80),
    },
    LeafColor {
        name: "Pine",
        rgb: Rgb(0x41, 0x6A, 0x5B),
    },
    LeafColor {
        name: "Dusty blue",
        rgb: Rgb(0x39, 0x67, 0x88),
    },
    LeafColor {
        name: "Mauve",
        rgb: Rgb(0xA5, 0x64, 0x89),
    },
    LeafColor {
        name: "Rose",
        rgb: Rgb(0xB2, 0x4E, 0x61),
    },
    LeafColor {
        name: "Coral",
        rgb: Rgb(0xCC, 0x6B, 0x6D),
    },
    LeafColor {
        name: "Terracotta",
        rgb: Rgb(0xC1, 0x72, 0x69),
    },
];

/// A leaf avatar: shape and disc colour indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LeafPick {
    pub leaf: u8,
    pub color: u8,
}

impl LeafPick {
    pub fn shape(self) -> &'static Leaf {
        &LEAVES[(self.leaf % LEAF_COUNT) as usize]
    }

    pub fn disc(self) -> Rgb {
        COLORS[(self.color % COLOR_COUNT) as usize].rgb
    }
}

/// FNV-1a over the UTF-8 bytes, 64-bit.
pub fn fnv1a64(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The automatic pick for an agent id (the manifest README's rule):
/// `leaf = h mod 9`, `colour = (h div 9) mod 10`.
pub fn default_pick(seed: &str) -> LeafPick {
    let h = fnv1a64(seed);
    LeafPick {
        leaf: (h % u64::from(LEAF_COUNT)) as u8,
        color: ((h / u64::from(LEAF_COUNT)) % u64::from(COLOR_COUNT)) as u8,
    }
}

/// The sway angle of the idle loop at time `t` seconds, in radians: a smooth
/// ±`IDLE_DEGREES` wave with the leaf's own period, starting and ending at rest.
pub fn idle_angle(leaf: u8, t: f32) -> f32 {
    let period = LEAVES[(leaf % LEAF_COUNT) as usize].idle_secs;
    (t / period * std::f32::consts::TAU).sin() * IDLE_DEGREES.to_radians()
}

/// The gust angle `t` seconds after a hover start, in radians: a damped
/// spring that peaks near `GUST_DEGREES` and is back at rest by `GUST_SECS`.
pub fn gust_angle(t: f32) -> f32 {
    if !(0.0..GUST_SECS).contains(&t) {
        return 0.0;
    }
    let u = t / GUST_SECS;
    // Two and a half swings, exponentially damped, zero at both ends.
    let envelope = (1.0 - u) * (-3.0 * u).exp() * 4.0;
    (u * std::f32::consts::PI * 2.5).sin() * envelope.min(1.0) * GUST_DEGREES.to_radians()
}

/// The pick in effect: the owner's choices where valid, else the automatic one.
pub fn pick_for(seed: &str, leaf: Option<u8>, color: Option<u8>) -> LeafPick {
    let auto = default_pick(seed);
    LeafPick {
        leaf: leaf.filter(|l| *l < LEAF_COUNT).unwrap_or(auto.leaf),
        color: color.filter(|c| *c < COLOR_COUNT).unwrap_or(auto.color),
    }
}

/// The bubble-bar accent of a leaf agent: its disc colour, both appearances.
pub fn accent(pick: LeafPick) -> Accent {
    let rgb = pick.disc();
    Accent {
        dark: rgb,
        light: rgb,
    }
}

/// The `d` attribute of a leaf's single `<path>`.
pub fn path_data(leaf: u8) -> &'static str {
    svg_path_data(LEAVES[(leaf % LEAF_COUNT) as usize].svg).unwrap_or("")
}

/// The first `<path … d="…">` of an SVG document.
pub fn svg_path_data(svg: &str) -> Option<&str> {
    let path = svg.find("<path")?;
    let rest = &svg[path..];
    let end = rest.find("/>").or_else(|| rest.find('>'))?;
    let tag = &rest[..end];
    let d = tag.find(" d=\"")?;
    let after = &tag[d + 4..];
    let close = after.find('"')?;
    Some(&after[..close])
}

/// One absolute outline command in the 100 × 100 leaf box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    MoveTo {
        x: f32,
        y: f32,
    },
    LineTo {
        x: f32,
        y: f32,
    },
    CubicTo {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x: f32,
        y: f32,
    },
    Close,
}

/// A leaf's outline as absolute commands, parsed once. Always fill it with
/// the even-odd rule: the art uses cut-outs for veins and notches.
pub fn segments(leaf: u8) -> &'static [Segment] {
    static CACHE: OnceLock<Vec<Vec<Segment>>> = OnceLock::new();
    let all = CACHE.get_or_init(|| (0..LEAF_COUNT).map(|i| parse_path(path_data(i))).collect());
    &all[(leaf % LEAF_COUNT) as usize]
}

/// Parse SVG path data into absolute move / line / cubic / close commands.
/// Handles every command letter (H V S T Q A and the relative forms are
/// rewritten); malformed input stops the parse and keeps what was read.
pub fn parse_path(d: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut p = Tokens::new(d);
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    // Previous control point, for the S/T reflections.
    let mut last_cubic: Option<(f32, f32)> = None;
    let mut last_quad: Option<(f32, f32)> = None;
    let mut cmd = b'M';
    while let Some(tok) = p.next_command_or_number() {
        let mut implicit = false;
        match tok {
            Token::Command(c) => cmd = c,
            Token::Number => {
                implicit = true;
                p.unread();
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        if implicit && up == b'M' {
            // Repeated coordinates after a move-to are line-tos.
            cmd = if rel { b'l' } else { b'L' };
        }
        let up = cmd.to_ascii_uppercase();
        let abs = |v: f32, base: f32| if rel { base + v } else { v };
        match up {
            b'M' => {
                let Some((x, y)) = p.pair() else { break };
                cx = abs(x, cx);
                cy = abs(y, cy);
                sx = cx;
                sy = cy;
                out.push(Segment::MoveTo { x: cx, y: cy });
                last_cubic = None;
                last_quad = None;
            }
            b'L' => {
                let Some((x, y)) = p.pair() else { break };
                cx = abs(x, cx);
                cy = abs(y, cy);
                out.push(Segment::LineTo { x: cx, y: cy });
                last_cubic = None;
                last_quad = None;
            }
            b'H' => {
                let Some(x) = p.number() else { break };
                cx = abs(x, cx);
                out.push(Segment::LineTo { x: cx, y: cy });
                last_cubic = None;
                last_quad = None;
            }
            b'V' => {
                let Some(y) = p.number() else { break };
                cy = abs(y, cy);
                out.push(Segment::LineTo { x: cx, y: cy });
                last_cubic = None;
                last_quad = None;
            }
            b'C' => {
                let (Some((x1, y1)), Some((x2, y2)), Some((x, y))) = (p.pair(), p.pair(), p.pair())
                else {
                    break;
                };
                let (x1, y1, x2, y2) = (abs(x1, cx), abs(y1, cy), abs(x2, cx), abs(y2, cy));
                cx = abs(x, cx);
                cy = abs(y, cy);
                out.push(Segment::CubicTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x: cx,
                    y: cy,
                });
                last_cubic = Some((x2, y2));
                last_quad = None;
            }
            b'S' => {
                let (Some((x2, y2)), Some((x, y))) = (p.pair(), p.pair()) else {
                    break;
                };
                let (x1, y1) = match last_cubic {
                    Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                    None => (cx, cy),
                };
                let (x2, y2) = (abs(x2, cx), abs(y2, cy));
                cx = abs(x, cx);
                cy = abs(y, cy);
                out.push(Segment::CubicTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x: cx,
                    y: cy,
                });
                last_cubic = Some((x2, y2));
                last_quad = None;
            }
            b'Q' | b'T' => {
                let (qx, qy) = if up == b'Q' {
                    let Some((x1, y1)) = p.pair() else { break };
                    (abs(x1, cx), abs(y1, cy))
                } else {
                    match last_quad {
                        Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        None => (cx, cy),
                    }
                };
                let Some((x, y)) = p.pair() else { break };
                let (nx, ny) = (abs(x, cx), abs(y, cy));
                // Degree elevation: quadratic → cubic.
                let x1 = cx + 2.0 / 3.0 * (qx - cx);
                let y1 = cy + 2.0 / 3.0 * (qy - cy);
                let x2 = nx + 2.0 / 3.0 * (qx - nx);
                let y2 = ny + 2.0 / 3.0 * (qy - ny);
                cx = nx;
                cy = ny;
                out.push(Segment::CubicTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x: cx,
                    y: cy,
                });
                last_quad = Some((qx, qy));
                last_cubic = None;
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some((x, y))) = (
                    p.number(),
                    p.number(),
                    p.number(),
                    p.flag(),
                    p.flag(),
                    p.pair(),
                ) else {
                    break;
                };
                let (nx, ny) = (abs(x, cx), abs(y, cy));
                arc_to_cubics(&mut out, cx, cy, rx, ry, rot, large, sweep, nx, ny);
                cx = nx;
                cy = ny;
                last_cubic = None;
                last_quad = None;
            }
            b'Z' => {
                out.push(Segment::Close);
                cx = sx;
                cy = sy;
                last_cubic = None;
                last_quad = None;
            }
            _ => break,
        }
    }
    out
}

/// SVG arc → cubic Béziers (W3C implementation notes F.6.5 / F.6.6).
#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    out: &mut Vec<Segment>,
    x0: f32,
    y0: f32,
    rx: f32,
    ry: f32,
    rot_deg: f32,
    large: bool,
    sweep: bool,
    x: f32,
    y: f32,
) {
    if (x0 - x).abs() < f32::EPSILON && (y0 - y).abs() < f32::EPSILON {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < f32::EPSILON || ry < f32::EPSILON {
        out.push(Segment::LineTo { x, y });
        return;
    }
    let phi = rot_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();
    let dx2 = (x0 - x) / 2.0;
    let dy2 = (y0 - y) / 2.0;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den <= 0.0 {
        0.0
    } else {
        (num / den).max(0.0).sqrt()
    };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * (rx * y1p / ry);
    let cyp = coef * -(ry * x1p / rx);
    let cx = cos_phi * cxp - sin_phi * cyp + (x0 + x) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (y0 + y) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = angle(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f32::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f32::consts::TAU;
    }
    let n = ((dtheta.abs() / (std::f32::consts::FRAC_PI_2)).ceil() as usize).max(1);
    let delta = dtheta / n as f32;
    let t = 4.0 / 3.0 * (delta / 4.0).tan();
    let mut th = theta1;
    for _ in 0..n {
        let (s1, c1) = th.sin_cos();
        let (s2, c2) = (th + delta).sin_cos();
        let e1 = (rx * (c1 - t * s1), ry * (s1 + t * c1));
        let e2 = (rx * (c2 + t * s2), ry * (s2 - t * c2));
        let end = (rx * c2, ry * s2);
        let map = |(px, py): (f32, f32)| {
            (
                cos_phi * px - sin_phi * py + cx,
                sin_phi * px + cos_phi * py + cy,
            )
        };
        let (x1, y1) = map(e1);
        let (x2, y2) = map(e2);
        let (ex, ey) = map(end);
        out.push(Segment::CubicTo {
            x1,
            y1,
            x2,
            y2,
            x: ex,
            y: ey,
        });
        th += delta;
    }
}

enum Token {
    Command(u8),
    Number,
}

struct Tokens<'a> {
    bytes: &'a [u8],
    pos: usize,
    last: usize,
}

impl<'a> Tokens<'a> {
    fn new(d: &'a str) -> Self {
        Self {
            bytes: d.as_bytes(),
            pos: 0,
            last: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_whitespace() || self.bytes[self.pos] == b',')
        {
            self.pos += 1;
        }
    }

    fn unread(&mut self) {
        self.pos = self.last;
    }

    fn next_command_or_number(&mut self) -> Option<Token> {
        self.skip_ws();
        self.last = self.pos;
        let b = *self.bytes.get(self.pos)?;
        if b.is_ascii_alphabetic() {
            self.pos += 1;
            Some(Token::Command(b))
        } else {
            self.number().map(|_| Token::Number)
        }
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_ws();
        let start = self.pos;
        let b = self.bytes;
        let mut i = start;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let mut digits = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            return None;
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
                j += 1;
            }
            let exp_start = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > exp_start {
                i = j;
            }
        }
        let s = std::str::from_utf8(&b[start..i]).ok()?;
        self.pos = i;
        s.parse().ok()
    }

    fn pair(&mut self) -> Option<(f32, f32)> {
        Some((self.number()?, self.number()?))
    }

    /// Arc flags may be written without separators (`1 0` or `10`).
    fn flag(&mut self) -> Option<bool> {
        self.skip_ws();
        let b = *self.bytes.get(self.pos)?;
        match b {
            b'0' => {
                self.pos += 1;
                Some(false)
            }
            b'1' => {
                self.pos += 1;
                Some(true)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_leaf_is_one_path_in_a_100_box() {
        for leaf in LEAVES.iter() {
            assert!(
                leaf.svg.contains("viewBox=\"0 0 100 100\""),
                "{}",
                leaf.slug
            );
            assert_eq!(leaf.svg.matches("<path").count(), 1, "{}", leaf.slug);
            assert!(leaf.svg.contains("currentColor"), "{}", leaf.slug);
            let d = svg_path_data(leaf.svg).expect("d attribute");
            let segs = parse_path(d);
            assert!(segs.len() > 3, "{}", leaf.slug);
            assert!(matches!(segs[0], Segment::MoveTo { .. }), "{}", leaf.slug);
            assert!(segs.contains(&Segment::Close), "{}", leaf.slug);
            for s in &segs {
                let inside = |v: f32| (-5.0..=105.0).contains(&v);
                match *s {
                    Segment::MoveTo { x, y } | Segment::LineTo { x, y } => {
                        assert!(inside(x) && inside(y), "{} {:?}", leaf.slug, s)
                    }
                    Segment::CubicTo { x, y, .. } => {
                        assert!(inside(x) && inside(y), "{} {:?}", leaf.slug, s)
                    }
                    Segment::Close => {}
                }
            }
        }
        assert_eq!(segments(0).len(), parse_path(path_data(0)).len());
    }

    #[test]
    fn default_pick_is_stable_and_spread() {
        assert_eq!(default_pick("x"), default_pick("x"));
        let mut cells = std::collections::HashSet::new();
        for i in 0..2000 {
            let p = default_pick(&format!("01M50W7HPM0KCFVMQ32HXN{i:04}"));
            assert!(p.leaf < LEAF_COUNT && p.color < COLOR_COUNT);
            cells.insert((p.leaf, p.color));
        }
        assert_eq!(cells.len(), 90, "2000 ids should cover the whole grid");
        // Reference values other clients can check against.
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
        let h = fnv1a64("01M50W7HPM0KCFVMQ32HXNTN5S");
        let p = default_pick("01M50W7HPM0KCFVMQ32HXNTN5S");
        assert_eq!((p.leaf, p.color), ((h % 9) as u8, ((h / 9) % 10) as u8));
    }

    #[test]
    fn table_matches_the_manifest() {
        let m: serde_json::Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        let leaves = m["leaves"].as_array().unwrap();
        assert_eq!(leaves.len(), LEAF_COUNT as usize);
        for (i, (l, j)) in LEAVES.iter().zip(leaves).enumerate() {
            assert_eq!(j["id"].as_u64().unwrap(), i as u64 + 1);
            assert_eq!(j["svg"].as_str().unwrap(), format!("svg/{}.svg", l.slug));
            let pivot = j["pivot"].as_array().unwrap();
            assert!(
                (pivot[0].as_f64().unwrap() as f32 - l.pivot.0).abs() < 1e-3,
                "{}",
                l.slug
            );
            assert!(
                (pivot[1].as_f64().unwrap() as f32 - l.pivot.1).abs() < 1e-3,
                "{}",
                l.slug
            );
            assert_eq!(
                j["idle_frames"].as_f64().unwrap() as f32 / 60.0,
                l.idle_secs
            );
            assert_eq!(
                j["default_color"]["hex"].as_str().unwrap(),
                COLORS[l.default_color as usize].rgb.hex()
            );
        }
        let palette = m["palette"].as_array().unwrap();
        assert_eq!(palette.len(), COLOR_COUNT as usize);
        for (c, j) in COLORS.iter().zip(palette) {
            assert_eq!(j["hex"].as_str().unwrap(), c.rgb.hex());
            assert_eq!(j["name"].as_str().unwrap(), c.name);
        }
        assert_eq!(m["leaf_color"].as_str().unwrap(), LEAF_INK.hex());
        assert_eq!(
            m["avatar"]["leaf_fraction_of_circle"].as_f64().unwrap() as f32,
            LEAF_SCALE
        );
    }

    #[test]
    fn lottie_files_are_the_manifests() {
        let m: serde_json::Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        for (l, j) in LEAVES.iter().zip(m["leaves"].as_array().unwrap()) {
            for (src, key, frames) in [
                (l.idle_lottie, "idle", j["idle_frames"].as_f64().unwrap()),
                (l.gust_lottie, "gust", j["gust_frames"].as_f64().unwrap()),
            ] {
                assert_eq!(
                    j["lottie"][key].as_str().unwrap(),
                    format!("lottie/{}-{key}.json", l.slug)
                );
                let v: serde_json::Value = serde_json::from_str(src).unwrap();
                assert_eq!(v["fr"].as_f64().unwrap(), 60.0, "{} {key}", l.slug);
                assert_eq!(v["op"].as_f64().unwrap(), frames, "{} {key}", l.slug);
                assert_eq!(
                    (v["w"].as_f64().unwrap(), v["h"].as_f64().unwrap()),
                    (100.0, 100.0)
                );
                let layers = v["layers"].as_array().unwrap();
                assert_eq!(layers.len(), 1, "{} {key}: one shape layer", l.slug);
                assert_eq!(layers[0]["nm"].as_str().unwrap(), "leaf");
            }
        }
    }

    #[test]
    fn motion_curves_rest_at_the_ends() {
        assert!(gust_angle(0.0).abs() < 1e-6);
        assert!(gust_angle(GUST_SECS).abs() < 1e-6);
        assert!(gust_angle(-0.1).abs() < 1e-6);
        let peak = (0..100)
            .map(|i| gust_angle(i as f32 / 100.0).abs())
            .fold(0.0f32, f32::max);
        assert!(peak > 5.0_f32.to_radians() && peak <= GUST_DEGREES.to_radians() + 1e-4);
        for leaf in 0..LEAF_COUNT {
            let period = LEAVES[leaf as usize].idle_secs;
            assert!(idle_angle(leaf, 0.0).abs() < 1e-6);
            assert!(idle_angle(leaf, period).abs() < 1e-3);
            assert!(idle_angle(leaf, period / 4.0) > 2.9_f32.to_radians());
        }
    }

    #[test]
    fn overrides_apply_when_valid() {
        let auto = default_pick("seed");
        assert_eq!(pick_for("seed", None, None), auto);
        assert_eq!(
            pick_for("seed", Some(3), Some(7)),
            LeafPick { leaf: 3, color: 7 }
        );
        assert_eq!(pick_for("seed", Some(9), Some(10)), auto);
        assert_eq!(accent(LeafPick { leaf: 0, color: 0 }).dark.hex(), "#BCA368");
        assert_eq!(COLORS[9].rgb.hex(), "#C17269");
    }

    #[test]
    fn parser_handles_relative_shorthand_and_arcs() {
        let segs = parse_path("m10 10 h20 v20 h-20 z");
        assert_eq!(
            segs,
            vec![
                Segment::MoveTo { x: 10.0, y: 10.0 },
                Segment::LineTo { x: 30.0, y: 10.0 },
                Segment::LineTo { x: 30.0, y: 30.0 },
                Segment::LineTo { x: 10.0, y: 30.0 },
                Segment::Close,
            ]
        );
        let segs = parse_path("M0 0 Q10 10 20 0 T40 0");
        assert_eq!(segs.len(), 3);
        assert!(matches!(segs[2], Segment::CubicTo { x, y, .. } if x == 40.0 && y == 0.0));
        let segs = parse_path("M10 50 A40 40 0 1 1 90 50");
        let last = segs.last().copied().unwrap();
        assert!(
            matches!(last, Segment::CubicTo { x, y, .. } if (x - 90.0).abs() < 0.01 && (y - 50.0).abs() < 0.01)
        );
        // A half circle needs two quarter arcs.
        assert_eq!(segs.len(), 3);
        // Flags without separators, numbers glued by signs and dots.
        // Move, two quarter arcs, then two implicit line-tos.
        let segs = parse_path("M0 0A5 5 0 01 10 0L-1.5.5-2-2");
        assert_eq!(segs.len(), 5);
        assert_eq!(segs[4], Segment::LineTo { x: -2.0, y: -2.0 });
    }
}
