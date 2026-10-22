//! Lottie playback for the leaf avatars (D92 follow-on): each agent's leaf
//! plays Jacob's committed bodymovin files — the seamless idle loop and the
//! one-shot gust — instead of the native sway.
//!
//! GPUI has no Lottie player and cannot host one: `velato`'s renderer needs
//! a vello/wgpu GPU context beside GPUI's Metal renderer, and the rlottie /
//! thorvg bindings mean C++ builds on the EC2 runner and Linux CI. What works,
//! and is exact for these files, is to use `velato` as the **parser and
//! animation evaluator** (full bodymovin schema, keyframe easing, layer and
//! group transforms, fixed and animated geometry) and to **draw with GPUI's
//! own lyon tessellation** (`PathBuilder`: cubics, even-odd fill). Everything
//! the committed files use — one shape layer, a group, a fixed path with
//! cut-outs, a solid fill, an animated rotate / scale / skew about the stem
//! anchor — is covered; strokes, trims, repeaters, mattes and precomps are
//! ignored (a test in `arbos-core` keeps the asset folder inside that subset).
//! The fill colour is the app's (cream), i.e. the `**.leaf.Color` recolour.
//!
//! Cost: parsing is ~0.4 ms per file, done once per leaf and cached;
//! evaluating a frame is a few keyframe lookups; tessellating a 30–130 element
//! path at avatar size is well under a millisecond. Lists draw the static SVG
//! and play nothing; the profile hero loops the idle file; a gust is one shot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{
    canvas, point, prelude::*, px, Bounds, FillOptions, FillRule, Hsla, IntoElement, PathBuilder,
    PathStyle, Pixels, Point, Window,
};
use kurbo::{Affine, PathEl};
use velato::model::{Content, Shape};
use velato::Composition;

use crate::ui::avatars::leaf;

/// Which of a leaf's two files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Clip {
    Idle,
    Gust,
}

type Clips = Mutex<HashMap<(u8, Clip), Arc<Composition>>>;

fn clips() -> &'static Clips {
    static CLIPS: OnceLock<Clips> = OnceLock::new();
    CLIPS.get_or_init(Default::default)
}

/// The parsed composition for a leaf's clip, parsed once per process.
pub fn composition(leaf_index: u8, clip: Clip) -> Option<Arc<Composition>> {
    let key = (leaf_index % leaf::LEAF_COUNT, clip);
    if let Some(c) = clips().lock().ok()?.get(&key) {
        return Some(c.clone());
    }
    let l = &leaf::LEAVES[key.0 as usize];
    let src = match clip {
        Clip::Idle => l.idle_lottie,
        Clip::Gust => l.gust_lottie,
    };
    let comp = Arc::new(Composition::from_slice(src.as_bytes()).ok()?);
    clips().lock().ok()?.insert(key, comp.clone());
    Some(comp)
}

/// Seconds one play of the clip takes.
pub fn duration_secs(comp: &Composition) -> f32 {
    let frames = (comp.frames.end - comp.frames.start).max(1.0);
    (frames / comp.frame_rate.max(1.0)) as f32
}

/// The frame at `t` ∈ [0, 1] of one play.
fn frame_at(comp: &Composition, t: f32) -> f64 {
    let span = comp.frames.end - comp.frames.start;
    // The last frame equals the first for a seamless loop; stop just short of it.
    comp.frames.start + (f64::from(t.clamp(0.0, 1.0)) * span).min(span - 1e-3)
}

/// Collect the fill paths of the composition at `frame`, in the 100 × 100 box.
fn paths_at(comp: &Composition, frame: f64) -> Vec<Vec<PathEl>> {
    let mut out = Vec::new();
    for layer in &comp.layers {
        let Content::Shape(shapes) = &layer.content else {
            continue;
        };
        let base = *layer.transform.evaluate(frame).as_ref();
        collect(shapes, frame, base, &mut out);
    }
    out
}

/// Walk a shape group: geometry elements accumulate into one path per group,
/// transformed by the group's and the layer's affines, emitted when a fill
/// (`Draw` without a stroke) closes the group — the Lottie draw order.
fn collect(shapes: &[Shape], frame: f64, transform: Affine, out: &mut Vec<Vec<PathEl>>) {
    let mut pending: Vec<PathEl> = Vec::new();
    for shape in shapes {
        match shape {
            Shape::Group(inner, group_transform) => {
                let t = match group_transform {
                    Some(g) => transform * *g.transform.evaluate(frame).as_ref(),
                    None => transform,
                };
                collect(inner, frame, t, out);
            }
            Shape::Geometry(g) => {
                let mut els = Vec::new();
                g.evaluate(frame, &mut els);
                pending.extend(els.into_iter().map(|el| transform_el(el, transform)));
            }
            Shape::Draw(d) if d.stroke.is_none() => {
                if !pending.is_empty() {
                    out.push(pending.clone());
                }
            }
            Shape::Draw(_) | Shape::Repeater(_) | Shape::Trim(_) => {}
        }
    }
}

fn transform_el(el: PathEl, t: Affine) -> PathEl {
    match el {
        PathEl::MoveTo(p) => PathEl::MoveTo(t * p),
        PathEl::LineTo(p) => PathEl::LineTo(t * p),
        PathEl::QuadTo(a, p) => PathEl::QuadTo(t * a, t * p),
        PathEl::CurveTo(a, b, p) => PathEl::CurveTo(t * a, t * b, t * p),
        PathEl::ClosePath => PathEl::ClosePath,
    }
}

/// Paint the clip's frame at `t` ∈ [0, 1] into `bounds` (the 100 × 100 box
/// scaled to fit), filled even-odd with `colour`.
pub fn paint_frame(
    comp: &Composition,
    t: f32,
    bounds: Bounds<Pixels>,
    colour: Hsla,
    window: &mut Window,
) {
    let side = f32::from(bounds.size.width).min(f32::from(bounds.size.height));
    let scale = side / comp.width.max(1) as f32;
    let origin: Point<Pixels> = point(
        bounds.origin.x + (bounds.size.width - px(side)) / 2.0,
        bounds.origin.y + (bounds.size.height - px(side)) / 2.0,
    );
    let map = |p: kurbo::Point| -> Point<Pixels> {
        point(
            origin.x + px(p.x as f32 * scale),
            origin.y + px(p.y as f32 * scale),
        )
    };
    for els in paths_at(comp, frame_at(comp, t)) {
        // Even-odd: the leaf art uses cut-outs for veins and notches.
        let mut b = PathBuilder::fill().with_style(PathStyle::Fill(
            FillOptions::default().with_fill_rule(FillRule::EvenOdd),
        ));
        for el in els {
            match el {
                PathEl::MoveTo(p) => b.move_to(map(p)),
                PathEl::LineTo(p) => b.line_to(map(p)),
                PathEl::QuadTo(a, p) => b.curve_to(map(p), map(a)),
                PathEl::CurveTo(a, c, p) => b.cubic_bezier_to(map(p), map(a), map(c)),
                PathEl::ClosePath => b.close(),
            }
        }
        if let Ok(path) = b.build() {
            window.paint_path(path, colour);
        }
    }
}

/// A canvas element of `size` points showing the clip's frame at `t`.
pub fn frame_element(comp: Arc<Composition>, t: f32, size: f32, colour: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| paint_frame(&comp, t, bounds, colour, window),
    )
    .size(px(size))
    .flex_shrink_0()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_committed_clip_parses_and_rests_at_frame_zero() {
        for i in 0..leaf::LEAF_COUNT {
            for clip in [Clip::Idle, Clip::Gust] {
                let comp = composition(i, clip).expect("parses");
                assert_eq!(comp.frame_rate, 60.0);
                assert_eq!((comp.width, comp.height), (100, 100));
                let first = paths_at(&comp, comp.frames.start);
                assert!(!first.is_empty(), "leaf {i} {clip:?} has a fill");
                // Frame 0 is the rest pose: the layer transform is the identity.
                let a = *comp.layers[0]
                    .transform
                    .evaluate(comp.frames.start)
                    .as_ref();
                let c = a.as_coeffs();
                assert!(
                    (c[0] - 1.0).abs() < 1e-6 && c[1].abs() < 1e-6,
                    "{i} {clip:?} {c:?}"
                );
                // A seamless loop / a settled gust: the last frame is back at rest.
                let z = *comp.layers[0]
                    .transform
                    .evaluate(comp.frames.end - 1.0)
                    .as_ref();
                let zc = z.as_coeffs();
                assert!(zc[1].abs() < 0.02, "{i} {clip:?} end rotation {zc:?}");
                let dur = duration_secs(&comp);
                match clip {
                    Clip::Idle => assert!((2.2..=2.9).contains(&dur), "{i} idle {dur}s"),
                    Clip::Gust => assert!((dur - 1.0).abs() < 1e-3, "{i} gust {dur}s"),
                }
            }
        }
    }

    #[test]
    fn the_gust_peaks_near_nine_degrees() {
        let comp = composition(0, Clip::Gust).unwrap();
        let mut peak = 0.0f64;
        let mut f = comp.frames.start;
        while f < comp.frames.end {
            let c = comp.layers[0].transform.evaluate(f).as_ref().as_coeffs();
            peak = peak.max(c[1].atan2(c[0]).abs().to_degrees());
            f += 1.0;
        }
        assert!((8.0..=10.0).contains(&peak), "{peak}");
    }
}
