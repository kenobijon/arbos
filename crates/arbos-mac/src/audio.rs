//! PCM helpers shared by the voice UI and the fixture source (F11): WAV
//! encode/decode in memory via `hound`, the bundled test clip, and a tone
//! synthesiser for the offline "spoken reply". No GPUI, no devices.

use std::io::Cursor;

/// Capture and upload rate: 16 kHz mono 16-bit, what Whisper-style STT wants.
pub const RATE: u32 = 16_000;

/// A 4 s clip ("Hello Arbos. What is on my plate today?") used when the
/// machine has no microphone, so the whole voice path still runs.
pub static TEST_CLIP_WAV: &[u8] = include_bytes!("../assets/test-voice.wav");

/// Decoded PCM, mono.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    pub fn duration_secs(&self) -> f32 {
        if self.rate == 0 {
            0.0
        } else {
            self.samples.len() as f32 / self.rate as f32
        }
    }
}

/// Encode 16-bit mono samples as a WAV blob.
pub fn encode_wav(rate: u32, samples: &[i16]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::with_capacity(44 + samples.len() * 2));
    {
        let mut writer = match hound::WavWriter::new(&mut cursor, spec) {
            Ok(w) => w,
            Err(_) => return Vec::new(),
        };
        for s in samples {
            let _ = writer.write_sample(*s);
        }
        let _ = writer.finalize();
    }
    cursor.into_inner()
}

/// Decode a WAV blob to mono 16-bit PCM (channels are averaged; floats and
/// other bit depths are rescaled). `None` when the bytes are not a WAV.
/// A streamed header (placeholder RIFF/`data` sizes, what OpenAI's TTS sends)
/// is repaired first; hound rejects it as is.
pub fn decode_wav(bytes: &[u8]) -> Option<Pcm> {
    if arbos_core::wav::needs_repair(bytes) {
        let mut fixed = bytes.to_vec();
        arbos_core::wav::repair_sizes(&mut fixed);
        return decode_wav_strict(&fixed);
    }
    decode_wav_strict(bytes)
}

fn decode_wav_strict(bytes: &[u8]) -> Option<Pcm> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).ok()?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mono: Vec<i16> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let shift = spec.bits_per_sample.saturating_sub(16) as u32;
            let scale_up = 16u32.saturating_sub(spec.bits_per_sample as u32);
            let all: Vec<i32> = reader.samples::<i32>().filter_map(Result::ok).collect();
            all.chunks(channels)
                .map(|frame| {
                    let sum: i64 = frame
                        .iter()
                        .map(|s| ((*s >> shift) << scale_up) as i64)
                        .sum();
                    (sum / frame.len() as i64).clamp(i16::MIN as i64, i16::MAX as i64) as i16
                })
                .collect()
        }
        hound::SampleFormat::Float => {
            let all: Vec<f32> = reader.samples::<f32>().filter_map(Result::ok).collect();
            all.chunks(channels)
                .map(|frame| {
                    let avg = frame.iter().sum::<f32>() / frame.len() as f32;
                    (avg.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                })
                .collect()
        }
    };
    Some(Pcm {
        rate: spec.sample_rate,
        samples: mono,
    })
}

/// Root-mean-square level of a frame, 0.0–1.0.
pub fn rms(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame
        .iter()
        .map(|s| {
            let x = *s as f64 / i16::MAX as f64;
            x * x
        })
        .sum();
    (sum / frame.len() as f64).sqrt() as f32
}

/// Nearest-sample resample to `to` Hz (good enough for speech going to STT).
pub fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || from == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let n = (samples.len() as u64 * to as u64 / from as u64) as usize;
    (0..n)
        .map(|i| {
            let src = (i as u64 * from as u64 / to as u64) as usize;
            samples[src.min(samples.len() - 1)]
        })
        .collect()
}

/// Offline "speech": one short tone per word, pitch following the word's
/// letters, so the fixture's spoken reply is audible and clearly synthetic.
pub fn tone_speech(text: &str) -> Vec<u8> {
    const WORD_SECS: f32 = 0.14;
    const GAP_SECS: f32 = 0.04;
    const MAX_SECS: f32 = 6.0;
    let mut out: Vec<i16> = Vec::new();
    let mut phase = 0.0f32;
    for word in text.split_whitespace() {
        if out.len() as f32 / RATE as f32 > MAX_SECS {
            break;
        }
        let seed: u32 = word
            .bytes()
            .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        let hz = 180.0 + (seed % 160) as f32;
        let n = (WORD_SECS * RATE as f32) as usize;
        for i in 0..n {
            let t = i as f32 / n as f32;
            let env = (t * 8.0).min(1.0).min((1.0 - t) * 8.0);
            phase += 2.0 * std::f32::consts::PI * hz / RATE as f32;
            let s = phase.sin() * 0.28 * env;
            out.push((s * i16::MAX as f32) as i16);
        }
        out.extend(std::iter::repeat_n(0i16, (GAP_SECS * RATE as f32) as usize));
    }
    if out.is_empty() {
        out.extend(std::iter::repeat_n(0i16, RATE as usize / 4));
    }
    encode_wav(RATE, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `POST /audio/speech` (gpt-4o-mini-tts, `wav`) really returns: 24 kHz mono
    /// 16-bit with RIFF and `data` sizes left at `0xFFFFFFFF`.
    fn streamed_like_openai(samples: &[i16]) -> Vec<u8> {
        let mut b = encode_wav(24_000, samples);
        b[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        b[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        b
    }

    #[test]
    fn hound_alone_rejects_the_streamed_header() {
        let b = streamed_like_openai(&[0i16; 2400]);
        assert!(hound::WavReader::new(Cursor::new(&b)).is_err());
        assert!(decode_wav_strict(&b).is_none());
    }

    #[test]
    fn decode_wav_accepts_the_streamed_header() {
        let samples: Vec<i16> = (0..24_000).map(|i| ((i % 100) as i16 - 50) * 100).collect();
        let b = streamed_like_openai(&samples);
        let pcm = decode_wav(&b).expect("streamed WAV decodes");
        assert_eq!(pcm.rate, 24_000);
        assert_eq!(pcm.samples, samples);
        assert!((pcm.duration_secs() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decode_wav_roundtrips_a_well_formed_file() {
        let samples: Vec<i16> = (0..1600).map(|i| (i * 7 % 3000) as i16).collect();
        let pcm = decode_wav(&encode_wav(RATE, &samples)).expect("wav");
        assert_eq!(pcm.rate, RATE);
        assert_eq!(pcm.samples, samples);
        assert!(decode_wav(b"not audio at all").is_none());
    }
}
