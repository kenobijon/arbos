//! RIFF/WAVE header repair for streamed speech.
//!
//! OpenAI's `/audio/speech` with `response_format: "wav"` (gpt-4o-mini-tts) streams the file
//! and never goes back to fill in the sizes: the RIFF chunk size and the `data` chunk size are
//! the placeholder `0xFFFF_FFFF`. Strict decoders (hound on the Mac, and therefore rodio's
//! WAV path) reject that header ("data chunk length is not a multiple of sample size"), so
//! the reply row showed "Reply audio could not be decoded" while the bytes were fine.
//! Once the whole body is in memory the real sizes are known; this writes them back.

const RIFF: &[u8; 4] = b"RIFF";
const WAVE: &[u8; 4] = b"WAVE";
const FMT: &[u8; 4] = b"fmt ";
const DATA: &[u8; 4] = b"data";
const UNKNOWN: u32 = u32::MAX;

/// What the header looks like once it is parsed (sizes as declared, not as repaired).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WavHeader {
    pub riff_size: u32,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    /// Byte offset of the first `data` sample.
    pub data_offset: usize,
    pub data_size: u32,
}

impl WavHeader {
    /// Bytes per frame (one sample per channel).
    pub fn block_align(&self) -> usize {
        (self.channels as usize).max(1) * (self.bits_per_sample as usize).div_ceil(8).max(1)
    }
}

/// Parse the RIFF/WAVE header of `bytes`; `None` when it is not a WAV with a `fmt ` and a
/// `data` chunk. Chunk sizes are returned as declared, so a streamed header is still parsed.
pub fn parse_header(bytes: &[u8]) -> Option<WavHeader> {
    if bytes.len() < 12 || &bytes[0..4] != RIFF || &bytes[8..12] != WAVE {
        return None;
    }
    let riff_size = u32_at(bytes, 4)?;
    let mut off = 12usize;
    let mut fmt: Option<(u16, u32, u16)> = None;
    while off + 8 <= bytes.len() {
        let id = &bytes[off..off + 4];
        let size = u32_at(bytes, off + 4)?;
        let body = off + 8;
        if id == FMT {
            if body + 16 > bytes.len() {
                return None;
            }
            let channels = u16_at(bytes, body + 2)?;
            let sample_rate = u32_at(bytes, body + 4)?;
            let bits = u16_at(bytes, body + 14)?;
            fmt = Some((channels, sample_rate, bits));
        } else if id == DATA {
            let (channels, sample_rate, bits_per_sample) = fmt?;
            return Some(WavHeader {
                riff_size,
                channels,
                sample_rate,
                bits_per_sample,
                data_offset: body,
                data_size: size,
            });
        }
        // A streamed placeholder on a non-data chunk means the header is unusable.
        if size == UNKNOWN {
            return None;
        }
        off = body
            .checked_add(size as usize)?
            .checked_add((size & 1) as usize)?;
    }
    None
}

/// True when the declared sizes do not describe `bytes` (streamed placeholders, zero, or a
/// `data` length past the end of the buffer).
pub fn needs_repair(bytes: &[u8]) -> bool {
    let Some(h) = parse_header(bytes) else {
        return false;
    };
    let actual_data = bytes.len().saturating_sub(h.data_offset);
    let actual_riff = bytes.len().saturating_sub(8);
    h.data_size == UNKNOWN
        || h.data_size == 0
        || h.data_size as usize > actual_data
        || h.riff_size == UNKNOWN
        || h.riff_size == 0
        || h.riff_size as usize > actual_riff
}

/// Rewrite the RIFF and `data` chunk sizes from the buffer's real length when they are
/// placeholders or inconsistent, and drop a trailing partial frame. Returns true when the
/// bytes changed. Anything that is not a parsable WAV is left untouched.
pub fn repair_sizes(bytes: &mut Vec<u8>) -> bool {
    if !needs_repair(bytes) {
        return false;
    }
    let Some(h) = parse_header(bytes) else {
        return false;
    };
    let align = h.block_align();
    let mut data_len = bytes.len().saturating_sub(h.data_offset);
    data_len -= data_len % align;
    bytes.truncate(h.data_offset + data_len);
    let riff_len = bytes.len().saturating_sub(8);
    bytes[4..8].copy_from_slice(&(riff_len as u32).to_le_bytes());
    let size_at = h.data_offset - 4;
    bytes[size_at..size_at + 4].copy_from_slice(&(data_len as u32).to_le_bytes());
    true
}

/// Seconds of audio described by a (repaired) header, from the `data` size.
pub fn duration_secs(bytes: &[u8]) -> Option<f32> {
    let h = parse_header(bytes)?;
    let frames = h.data_size as usize / h.block_align();
    (h.sample_rate > 0).then(|| frames as f32 / h.sample_rate as f32)
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact header OpenAI streams for `gpt-4o-mini-tts` + `wav` (captured 2026-10-22):
    /// 24 kHz mono 16-bit PCM, RIFF size and data size both `0xFFFFFFFF`.
    fn openai_streamed_wav(frames: usize) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&24_000u32.to_le_bytes());
        b.extend_from_slice(&48_000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        for i in 0..frames {
            b.extend_from_slice(&((i % 200) as i16 * 50).to_le_bytes());
        }
        b
    }

    fn well_formed_wav(frames: usize) -> Vec<u8> {
        let mut b = openai_streamed_wav(frames);
        assert!(repair_sizes(&mut b));
        b
    }

    #[test]
    fn parses_the_streamed_header_as_declared() {
        let b = openai_streamed_wav(24_000);
        let h = parse_header(&b).expect("header");
        assert_eq!(h.riff_size, u32::MAX);
        assert_eq!(h.data_size, u32::MAX);
        assert_eq!(h.channels, 1);
        assert_eq!(h.sample_rate, 24_000);
        assert_eq!(h.bits_per_sample, 16);
        assert_eq!(h.data_offset, 44);
        assert!(needs_repair(&b));
    }

    #[test]
    fn repair_writes_real_sizes() {
        let mut b = openai_streamed_wav(24_000);
        assert!(repair_sizes(&mut b));
        let h = parse_header(&b).expect("header");
        assert_eq!(h.data_size as usize, 24_000 * 2);
        assert_eq!(h.riff_size as usize, b.len() - 8);
        assert!(!needs_repair(&b));
        assert_eq!(duration_secs(&b), Some(1.0));
        // Second pass is a no-op.
        assert!(!repair_sizes(&mut b));
    }

    #[test]
    fn repair_drops_a_trailing_partial_frame() {
        let mut b = openai_streamed_wav(100);
        b.push(0x7f);
        assert!(repair_sizes(&mut b));
        assert_eq!(b.len(), 44 + 200);
        assert_eq!(parse_header(&b).unwrap().data_size, 200);
    }

    #[test]
    fn well_formed_is_left_alone() {
        let mut b = well_formed_wav(500);
        let before = b.clone();
        assert!(!needs_repair(&b));
        assert!(!repair_sizes(&mut b));
        assert_eq!(b, before);
    }

    #[test]
    fn zero_sizes_are_repaired_too() {
        let mut b = openai_streamed_wav(10);
        b[4..8].copy_from_slice(&0u32.to_le_bytes());
        b[40..44].copy_from_slice(&0u32.to_le_bytes());
        assert!(needs_repair(&b));
        assert!(repair_sizes(&mut b));
        assert_eq!(parse_header(&b).unwrap().data_size, 20);
    }

    #[test]
    fn non_wav_bytes_are_untouched() {
        for raw in [
            Vec::new(),
            b"ID3\x04\x00\x00\x00\x00\x00\x00mp3 frame".to_vec(),
            b"RIFF\xff\xff\xff\xffWAVEfmt ".to_vec(),
            b"{\"error\":\"nope\"}".to_vec(),
        ] {
            let mut b = raw.clone();
            assert!(parse_header(&b).is_none());
            assert!(!needs_repair(&b));
            assert!(!repair_sizes(&mut b));
            assert_eq!(b, raw);
        }
    }

    #[test]
    fn skips_list_chunks_before_data() {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes()); // stereo
        b.extend_from_slice(&16_000u32.to_le_bytes());
        b.extend_from_slice(&64_000u32.to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"LIST");
        b.extend_from_slice(&5u32.to_le_bytes());
        b.extend_from_slice(b"INFOx\0"); // 5 bytes + pad
        b.extend_from_slice(b"data");
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        b.extend_from_slice(&[0u8; 4 * 1600 + 3]); // 1600 frames + a partial one
        assert!(repair_sizes(&mut b));
        let h = parse_header(&b).unwrap();
        assert_eq!(h.channels, 2);
        assert_eq!(h.block_align(), 4);
        assert_eq!(h.data_size, 4 * 1600);
        assert_eq!(duration_secs(&b), Some(0.1));
    }
}
