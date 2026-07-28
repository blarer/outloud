//! Minimal RIFF/WAVE reader for the file-driven test mode.
//!
//! Exists so `--once --wav file.wav` can exercise every pipeline stage
//! except capture on machines with no usable microphone (the task's
//! verification requirement). Supports the formats `afconvert` and `sox`
//! actually emit for speech: PCM 16-bit and IEEE float 32-bit, any channel
//! count, any rate (we downmix and resample to the pipeline's 16kHz mono).
//! Not a general WAV library on purpose: unknown chunks are skipped, exotic
//! encodings are a named error, and nothing here is a dependency.

use std::path::Path;

use audio::resample::{downmix, Resampler};
use audio::SAMPLE_RATE;

/// Load a WAV file as 16kHz mono f32, the pipeline's lingua franca.
pub fn load_16k_mono(path: &Path) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(
        bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "{} is not a RIFF/WAVE file",
        path.display()
    );

    // Walk the chunk list: fmt then data. Other chunks (LIST, fact, cue)
    // are skipped, because encoders scatter them freely.
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (format, channels, rate, bits)
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_end = (pos + 8 + size).min(bytes.len());
        let body = &bytes[pos + 8..body_end];
        match id {
            b"fmt " if body.len() >= 16 => {
                fmt = Some((
                    u16::from_le_bytes(body[0..2].try_into().unwrap()),
                    u16::from_le_bytes(body[2..4].try_into().unwrap()),
                    u32::from_le_bytes(body[4..8].try_into().unwrap()),
                    u16::from_le_bytes(body[14..16].try_into().unwrap()),
                ));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        // Chunks are word-aligned; odd sizes carry a pad byte.
        pos = pos + 8 + size + (size & 1);
    }

    let (format, channels, rate, bits) =
        fmt.ok_or_else(|| anyhow::anyhow!("{}: no fmt chunk", path.display()))?;
    let data = data.ok_or_else(|| anyhow::anyhow!("{}: no data chunk", path.display()))?;

    // WAVE_FORMAT_EXTENSIBLE (0xFFFE) carries the real format in a
    // subformat GUID; for our two supported encodings the bit depth alone
    // disambiguates, so accept it rather than failing on afconvert output.
    let samples: Vec<f32> = match (format, bits) {
        (1 | 0xFFFE, 16) => data
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect(),
        (3 | 0xFFFE, 32) => data
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        _ => anyhow::bail!(
            "{}: unsupported WAV encoding (format {format}, {bits}-bit); \
             convert with `afconvert -f WAVE -d LEI16@16000 -c 1 in out.wav`",
            path.display()
        ),
    };

    let mono = downmix(&samples, channels.max(1) as usize);
    if rate == SAMPLE_RATE {
        return Ok(mono);
    }
    let mut resampler = Resampler::new(rate, SAMPLE_RATE);
    Ok(resampler.process(&mono))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory 16-bit PCM WAV.
    fn wav_i16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    #[test]
    fn reads_16k_mono_pcm16_unchanged() {
        let dir = std::env::temp_dir().join("outloud-wav-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.wav");
        std::fs::write(&p, wav_i16(16_000, 1, &[0, 16384, -16384])).unwrap();
        let s = load_16k_mono(&p).unwrap();
        assert_eq!(s.len(), 3);
        assert!((s[1] - 0.5).abs() < 1e-3);
        assert!((s[2] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn resamples_48k_stereo_to_16k_mono() {
        let dir = std::env::temp_dir().join("outloud-wav-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("b.wav");
        // 48000 stereo frames = 1s -> ~16000 mono samples.
        let frames: Vec<i16> = std::iter::repeat_n([1000i16, -1000i16], 48_000)
            .flatten()
            .collect();
        std::fs::write(&p, wav_i16(48_000, 2, &frames)).unwrap();
        let s = load_16k_mono(&p).unwrap();
        assert!((15_900..=16_100).contains(&s.len()), "got {}", s.len());
        // Stereo +/- downmixes to ~0.
        assert!(s.iter().all(|v| v.abs() < 1e-3));
    }

    #[test]
    fn rejects_non_wav_with_named_error() {
        let dir = std::env::temp_dir().join("outloud-wav-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.wav");
        std::fs::write(&p, b"not a wav at all").unwrap();
        assert!(load_16k_mono(&p).is_err());
    }
}
