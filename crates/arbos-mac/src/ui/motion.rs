//! Shared 30 fps pulse clock for streaming dots and spinners (zeron-study
//! §2.6): one timer drives every pulsing view instead of per-frame redraws.
//! A view that needs the next tick calls [`PulseClock::request`] while it
//! renders; when nobody asks, the clock stays idle.

use std::time::{Duration, Instant};

use gpui::{App, Global};

pub const PULSE_FPS: u64 = 30;
pub const COMPOSER_DROP_MS: u64 = 220;
pub const SHAKE_MS: u64 = 120;

pub struct PulseClock {
    started: Instant,
    wanted: bool,
}

impl Global for PulseClock {}

impl PulseClock {
    pub fn init(cx: &mut App) {
        cx.set_global(PulseClock {
            started: Instant::now(),
            wanted: false,
        });
        cx.spawn(async move |cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(1000 / PULSE_FPS))
                .await;
            cx.update(|cx| {
                let wanted = std::mem::take(&mut cx.global_mut::<PulseClock>().wanted);
                if wanted {
                    for window in cx.windows() {
                        let _ = window.update(cx, |_, window, _| window.refresh());
                    }
                }
            });
        })
        .detach();
    }

    /// Ask for one more tick. Call from `render` while something animates.
    pub fn request(cx: &mut App) {
        cx.global_mut::<PulseClock>().wanted = true;
    }

    /// 0..1 triangle wave with the given period.
    pub fn phase(cx: &App, period_ms: u64) -> f32 {
        let elapsed = cx.global::<PulseClock>().started.elapsed().as_millis() as u64;
        let t = (elapsed % period_ms) as f32 / period_ms as f32;
        if t < 0.5 {
            t * 2.0
        } else {
            2.0 - t * 2.0
        }
    }
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u
}
