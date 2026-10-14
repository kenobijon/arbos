//! Streaming fade (zeron-study §4.4 step 8). Each appended byte range becomes
//! a chunk with an arrival time. Alpha is paint-only: the text layout never
//! changes, so nothing jumps.

use std::ops::Range;
use std::time::{Duration, Instant};

const MIN_FADE_MS: f32 = 120.0;
const MAX_FADE_MS: f32 = 400.0;

#[derive(Clone, Debug)]
struct Chunk {
    range: Range<usize>,
    at: Instant,
}

#[derive(Clone, Debug)]
pub struct Veil {
    chunks: Vec<Chunk>,
    seen_len: usize,
    last_arrival: Option<Instant>,
    ema_gap_ms: f32,
}

impl Default for Veil {
    fn default() -> Self {
        Self::new()
    }
}

impl Veil {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            seen_len: 0,
            last_arrival: None,
            ema_gap_ms: 120.0,
        }
    }

    /// Seed with text that is already settled (no fade).
    pub fn seeded(len: usize) -> Self {
        let mut v = Self::new();
        v.seen_len = len;
        v
    }

    /// Record that the text grew to `new_len` bytes.
    pub fn observe(&mut self, new_len: usize) {
        if new_len <= self.seen_len {
            self.seen_len = new_len;
            self.chunks.retain(|c| c.range.start < new_len);
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.last_arrival {
            let gap = now.duration_since(last).as_secs_f32() * 1000.0;
            self.ema_gap_ms = self.ema_gap_ms * 0.7 + gap * 0.3;
        }
        self.last_arrival = Some(now);
        self.chunks.push(Chunk {
            range: self.seen_len..new_len,
            at: now,
        });
        self.seen_len = new_len;
        let fade = self.fade_duration();
        self.chunks.retain(|c| now.duration_since(c.at) < fade);
    }

    fn fade_duration(&self) -> Duration {
        let ms = (self.ema_gap_ms * 3.0).clamp(MIN_FADE_MS, MAX_FADE_MS);
        Duration::from_millis(ms as u64)
    }

    /// Byte ranges still fading, with their alpha (0..1). Empty when settled.
    pub fn spans(&self, now: Instant) -> Vec<(Range<usize>, f32)> {
        let fade = self.fade_duration().as_secs_f32();
        self.chunks
            .iter()
            .filter_map(|c| {
                let p = now.duration_since(c.at).as_secs_f32() / fade;
                if p >= 1.0 {
                    None
                } else {
                    let alpha = 1.0 - (1.0 - p).powf(1.6);
                    Some((c.range.clone(), alpha))
                }
            })
            .collect()
    }

    pub fn is_active(&self, now: Instant) -> bool {
        !self.spans(now).is_empty()
    }
}
