//! Call tones (calls v1): the ringback a caller hears while nobody has
//! picked up, and the ring a callee hears while the ring card is up. Both
//! are synthesised, play through the system output at the Mac's volume (a
//! plain rodio sink on the default device, which CoreAudio shares with
//! WebRTC's device module), and loop until dropped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rodio::buffer::SamplesBuffer;
use rodio::Source;

const RATE: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// US ringback: 440 Hz + 480 Hz, 2 s on, 4 s off.
    Ringback,
    /// Incoming call: a two-note chime, twice, then a pause.
    Ring,
}

/// One looping tone. Dropping it stops the sound.
pub struct TonePlayer {
    stop: Arc<AtomicBool>,
}

impl TonePlayer {
    pub fn start(tone: Tone) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let samples = render(tone);
        // rodio's output stream is not Send: it lives on its own thread for
        // as long as the tone plays.
        let _ = std::thread::Builder::new()
            .name(format!("arbos-tone-{tone:?}"))
            .spawn(move || {
                let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
                    return;
                };
                let Ok(sink) = rodio::Sink::try_new(&handle) else {
                    return;
                };
                sink.append(SamplesBuffer::new(1, RATE, samples).repeat_infinite());
                sink.play();
                while !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(40));
                }
                sink.stop();
            });
        Self { stop }
    }
}

impl Drop for TonePlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One full cycle of the tone as 48 kHz mono samples.
pub fn render(tone: Tone) -> Vec<f32> {
    match tone {
        Tone::Ringback => {
            let on = (RATE * 2) as usize;
            let off = (RATE * 4) as usize;
            let mut out = Vec::with_capacity(on + off);
            for i in 0..on {
                let t = i as f32 / RATE as f32;
                let env = ramp(i, on, RATE as usize / 100);
                let s = (two_pi(440.0, t).sin() + two_pi(480.0, t).sin()) * 0.5;
                out.push(s * 0.28 * env);
            }
            out.resize(on + off, 0.0);
            out
        }
        Tone::Ring => {
            // "ding-dong" twice (0.9 s), then 2.6 s quiet: a 3.5 s cycle.
            let note = |out: &mut Vec<f32>, hz: f32, secs: f32| {
                let n = (RATE as f32 * secs) as usize;
                for i in 0..n {
                    let t = i as f32 / RATE as f32;
                    // Soft bell: fundamental + a quiet octave, exponential decay.
                    let decay = (-t * 6.0).exp();
                    let s = two_pi(hz, t).sin() * 0.8 + two_pi(hz * 2.0, t).sin() * 0.2;
                    out.push(s * 0.30 * decay * ramp(i, n, 240));
                }
            };
            let mut out = Vec::with_capacity((RATE as f32 * 3.5) as usize);
            for _ in 0..2 {
                note(&mut out, 1046.5, 0.22); // C6
                note(&mut out, 783.99, 0.23); // G5
            }
            out.resize((RATE as f32 * 3.5) as usize, 0.0);
            out
        }
    }
}

fn two_pi(hz: f32, t: f32) -> f32 {
    2.0 * std::f32::consts::PI * hz * t
}

/// Linear fade at both ends so the tone never clicks.
fn ramp(i: usize, n: usize, edge: usize) -> f32 {
    let edge = edge.max(1).min(n / 2);
    if i < edge {
        i as f32 / edge as f32
    } else if i + edge > n {
        (n - i) as f32 / edge as f32
    } else {
        1.0
    }
}

/// Plays one cached voice sample file through the system output, once.
/// Dropping it (or starting another) stops playback.
pub struct SamplePlayer {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl SamplePlayer {
    pub fn play(path: std::path::PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (flag, finished) = (stop.clone(), done.clone());
        let _ = std::thread::Builder::new()
            .name("arbos-voice-sample".into())
            .spawn(move || {
                let Ok(file) = std::fs::File::open(&path) else {
                    finished.store(true, Ordering::Relaxed);
                    return;
                };
                let Ok(source) = rodio::Decoder::new(std::io::BufReader::new(file)) else {
                    finished.store(true, Ordering::Relaxed);
                    return;
                };
                let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
                    finished.store(true, Ordering::Relaxed);
                    return;
                };
                let Ok(sink) = rodio::Sink::try_new(&handle) else {
                    finished.store(true, Ordering::Relaxed);
                    return;
                };
                sink.append(source);
                sink.play();
                while !flag.load(Ordering::Relaxed) && !sink.empty() {
                    std::thread::sleep(Duration::from_millis(40));
                }
                sink.stop();
                finished.store(true, Ordering::Relaxed);
            });
        Self { stop, done }
    }

    /// Still playing (the "stop" glyph on the row).
    pub fn is_playing(&self) -> bool {
        !self.done.load(Ordering::Relaxed)
    }
}

impl Drop for SamplePlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ringback_is_two_seconds_on_four_off() {
        let s = render(Tone::Ringback);
        assert_eq!(s.len(), (RATE * 6) as usize);
        let on = &s[..(RATE * 2) as usize];
        let off = &s[(RATE * 2) as usize..];
        assert!(on.iter().any(|v| v.abs() > 0.2));
        assert!(off.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn ring_cycle_has_sound_then_silence() {
        let s = render(Tone::Ring);
        assert!(s[..RATE as usize].iter().any(|v| v.abs() > 0.1));
        assert!(s[s.len() - RATE as usize..].iter().all(|v| *v == 0.0));
    }
}
