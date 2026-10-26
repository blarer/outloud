//! NVIDIA Nemotron ASR through the `nemo-speech` CLI.
//!
//! ## Why a subprocess rather than a library
//!
//! NeMo-Speech.cpp is ggml-based and does expose a C++ library, so linking
//! it the way [`super::whisper_cpp`] links whisper-rs is conceivable. It is
//! not done here, for reasons that are about this project rather than about
//! the library:
//!
//! - There is no `nemo-speech-rs` crate. Linking would mean hand-written
//!   FFI plus a build script that finds or builds the C++ tree, which is a
//!   large amount of unsafe surface for one backend on one platform.
//! - The CLI is the interface NVIDIA documents and tests. A GGUF path and
//!   a language flag is the whole contract, and it is stable across the
//!   model updates this project will want to follow.
//! - The measured cost is small. `bench asr` on a GB10 reports ~300ms to
//!   load the model and 15-20ms to decode three seconds of speech, so a
//!   process spawn is the dominant overhead only because decode is so
//!   cheap, not because spawning is expensive.
//!
//! The honest consequence is recorded in [`NemoRecognizer::reusable`]:
//! every utterance pays model load again, because each invocation is a
//! fresh process. That is the same tradeoff [`super::apple`] documents for
//! its helper process, and the trait exists to let callers see it rather
//! than discover it.
//!
//! ## Why this model at all
//!
//! The crate docs classify whisper.cpp without its streaming hack and
//! Parakeet TDT as batch-only engines. Nemotron 3.5 ASR is a cache-aware
//! streaming FastConformer-RNNT: it keeps encoder caches between chunks
//! instead of recomputing overlapping windows. It also emits punctuation
//! and capitalisation natively, which whisper needs a separate pass for.
//!
//! On the host this was developed against it is the only recognizer that
//! uses the GPU at all, and `nemo-speech` prints `backend=CUDA0` to say so.
//! That matters because a published DGX Spark writeup found faster-whisper
//! silently falling back to CPU int8 at one to three seconds per six-second
//! utterance, which is slower than useless for dictation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::{Partial, Recognizer, Transcript};

/// Sample rate the model expects. Audio arrives from the pipeline at this
/// rate already; writing a WAV header that claims anything else would make
/// the transcript wrong rather than fail loudly.
const SAMPLE_RATE: u32 = 16_000;

/// Where to look for the CLI when neither config nor environment names it.
///
/// The build directory is included deliberately: NeMo-Speech.cpp has no
/// install step in its documented quickstart, so the binary very often
/// lives exactly there and nowhere on `PATH`. Leaving it out would mean a
/// working build still reports "not found".
const CLI_CANDIDATES: &[&str] = &[
    "nemo-speech",
    "NeMo-Speech.cpp/build/cuda-asr/bin/nemo-speech",
    "NeMo-Speech.cpp/build/bin/nemo-speech",
];

pub struct NemoRecognizer {
    cli: PathBuf,
    model: PathBuf,
    language: String,
    /// Utterance audio, accumulated until `finalize`.
    ///
    /// The CLI transcribes a file, so there is nothing useful to do with a
    /// partial buffer: see `feed`.
    buffered: Vec<f32>,
}

impl NemoRecognizer {
    /// Locate the CLI and model, failing with an actionable message.
    ///
    /// Takes nothing: every input is discovered, because the alternative is
    /// a constructor whose arguments the CLI layer has to invent defaults
    /// for anyway.
    pub fn new() -> Result<Self> {
        let cli = discover_cli().context(
            "could not find the nemo-speech CLI. Build it from \
             https://github.com/NVIDIA/NeMo-Speech.cpp, or set \
             OUTLOUD_NEMO_CLI to the binary",
        )?;
        let model = discover_model().context(
            "could not find a Nemotron ASR GGUF. Fetch one:\n  \
             hf download nvidia/nemotron-3.5-asr-streaming-0.6b \\\n    \
             nemotron-3.5-asr-streaming-0.6b.q8_0.gguf --local-dir models\n\
             then set OUTLOUD_NEMO_MODEL to it",
        )?;

        Ok(Self {
            cli,
            model,
            language: std::env::var("OUTLOUD_NEMO_LANGUAGE")
                .unwrap_or_else(|_| "en-US".to_string()),
            buffered: Vec::new(),
        })
    }

    /// Whether this backend could run, without constructing it.
    ///
    /// For `doctor` and for a settings UI that should not offer a backend
    /// the machine cannot use.
    pub fn available() -> bool {
        discover_cli().is_some() && discover_model().is_some()
    }

    /// Write `samples` as a 16-bit mono WAV the CLI can read.
    ///
    /// Hand-rolled rather than pulling in a WAV crate: the header is 44
    /// bytes and fully specified, and this is the only place in the crate
    /// that needs one.
    fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
        let mut file = std::fs::File::create(path)
            .with_context(|| format!("creating {}", path.display()))?;

        let data_bytes = (samples.len() * 2) as u32;
        let header_tail = 36 + data_bytes;

        file.write_all(b"RIFF")?;
        file.write_all(&header_tail.to_le_bytes())?;
        file.write_all(b"WAVEfmt ")?;
        file.write_all(&16u32.to_le_bytes())?; // fmt chunk size
        file.write_all(&1u16.to_le_bytes())?; // PCM
        file.write_all(&1u16.to_le_bytes())?; // mono
        file.write_all(&SAMPLE_RATE.to_le_bytes())?;
        file.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?; // byte rate
        file.write_all(&2u16.to_le_bytes())?; // block align
        file.write_all(&16u16.to_le_bytes())?; // bits per sample
        file.write_all(b"data")?;
        file.write_all(&data_bytes.to_le_bytes())?;

        for &sample in samples {
            // Clamp before casting: a sample above 1.0 would wrap to a
            // large negative value and arrive as a click loud enough to
            // change the transcript.
            let clamped = sample.clamp(-1.0, 1.0);
            let pcm = (clamped * i16::MAX as f32) as i16;
            file.write_all(&pcm.to_le_bytes())?;
        }

        Ok(())
    }

    /// Pull the transcript out of the CLI's stdout.
    ///
    /// The CLI interleaves progress lines with the transcript, so this
    /// cannot simply take all of stdout. Diagnostic lines are bracketed
    /// (`[asr] model=... backend=CUDA0`) or carry known key=value shapes,
    /// and the transcript is the remaining prose.
    fn parse_transcript(stdout: &str) -> String {
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| !line.starts_with('['))
            .filter(|line| !line.contains("session finished"))
            .filter(|line| !line.contains('='))
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    }
}

fn discover_cli() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("OUTLOUD_NEMO_CLI") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    for candidate in CLI_CANDIDATES {
        if candidate.contains('/') {
            if let Some(home) = &home {
                let path = home.join(candidate);
                if path.is_file() {
                    return Some(path);
                }
            }
        } else if let Some(found) = which(candidate) {
            return Some(found);
        }
    }
    None
}

fn discover_model() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("OUTLOUD_NEMO_MODEL") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }

    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dirs = [
        home.join("models/nemotron-3.5-asr"),
        home.join("models"),
        config::model_dir(),
    ];

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            if name.ends_with(".gguf") && name.contains("asr") {
                return Some(path);
            }
        }
    }
    None
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|candidate| candidate.is_file())
    })
}

impl Recognizer for NemoRecognizer {
    /// Buffer only.
    ///
    /// Returning `None` always is deliberate and matches
    /// [`super::whisper_cpp`]: the CLI transcribes a whole file, so
    /// producing a partial would mean spawning a process per audio chunk
    /// and paying ~300ms of model load each time, to show a hypothesis that
    /// the final pass may contradict.
    ///
    /// The model itself is a streaming architecture and the CLI has a
    /// `--mode stream` benchmark path, so a genuinely incremental backend
    /// is possible later. It needs a resident process with a protocol,
    /// which is a different design from this one, not an adjustment to it.
    fn feed(&mut self, samples: &[f32]) -> Option<Partial> {
        self.buffered.extend_from_slice(samples);
        None
    }

    fn finalize(&mut self) -> Result<Transcript> {
        if self.buffered.is_empty() {
            return Ok(Transcript::empty());
        }

        // A named temp file rather than stdin: the CLI takes a path, and
        // relying on /dev/stdin would break the seek a WAV reader does.
        let wav = std::env::temp_dir().join(format!("outloud-nemo-{}.wav", std::process::id()));
        Self::write_wav(&wav, &self.buffered)?;
        let sample_count = self.buffered.len();
        self.buffered.clear();

        let output = Command::new(&self.cli)
            .arg("transcribe")
            .arg(&wav)
            .arg("--model")
            .arg(&self.model)
            .arg("--language")
            .arg(&self.language)
            .output()
            .with_context(|| format!("running {}", self.cli.display()));

        // Remove the temp file whether or not the CLI succeeded, but do not
        // let a failed removal mask a transcription error.
        let _ = std::fs::remove_file(&wav);
        let output = output?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "nemo-speech failed ({}): {}",
                output.status,
                stderr.lines().last().unwrap_or("no stderr").trim()
            );
        }

        let text = Self::parse_transcript(&String::from_utf8_lossy(&output.stdout));
        Ok(Transcript {
            text,
            // The CLI reports no per-word timings, and `words` is
            // optional-by-empty rather than Option, so an empty vec is the
            // correct "not available" rather than a missing feature.
            words: Vec::new(),
            audio_secs: sample_count as f32 / SAMPLE_RATE as f32,
        })
    }

    /// True: nothing in this backend is consumed by finalizing.
    ///
    /// Unlike [`super::apple`], whose helper process ends when its stdin
    /// closes, this spawns a fresh process per utterance and holds no
    /// state between them. The cost of that choice is paying model load
    /// every time, which is ~300ms on the host this was measured on.
    fn reusable(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "nemo"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_is_separated_from_the_cli_diagnostics() {
        // Real output shape, including the backend line that proves GPU use
        // and the trailing session line.
        let stdout = "\
[asr] model=nemotron-3.5-asr-streaming-0.6b.q8_0 head=rnnt backend=CUDA0
attention-right=3 encoder-frames
The quick brown fox jumps over the lazy dog.
[nemo-speech] transcribe session finished
";
        assert_eq!(
            NemoRecognizer::parse_transcript(stdout),
            "The quick brown fox jumps over the lazy dog."
        );
    }

    #[test]
    fn empty_cli_output_yields_an_empty_transcript_not_a_panic() {
        assert_eq!(NemoRecognizer::parse_transcript(""), "");
        assert_eq!(NemoRecognizer::parse_transcript("[asr] only noise\n"), "");
    }

    #[test]
    fn wav_header_describes_the_payload_it_is_attached_to() {
        let dir = std::env::temp_dir().join("outloud-nemo-wav-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one.wav");

        // 4 samples -> 8 bytes of PCM, so RIFF size is 36 + 8.
        NemoRecognizer::write_wav(&path, &[0.0, 0.5, -0.5, 1.0]).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            36 + 8,
            "RIFF chunk size must cover the header tail plus the samples"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            SAMPLE_RATE,
            "a wrong sample rate here silently changes the transcript"
        );
        assert_eq!(bytes.len(), 44 + 8, "44-byte header plus 4 i16 samples");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn samples_past_full_scale_clamp_instead_of_wrapping() {
        let dir = std::env::temp_dir().join("outloud-nemo-clamp-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.wav");

        // 2.0 would overflow an i16 cast and wrap to a large negative
        // value, which the model hears as a click.
        NemoRecognizer::write_wav(&path, &[2.0, -2.0]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let first = i16::from_le_bytes(bytes[44..46].try_into().unwrap());
        let second = i16::from_le_bytes(bytes[46..48].try_into().unwrap());

        assert!(first > 32_000, "positive overflow must clamp high, got {first}");
        assert!(second < -32_000, "negative overflow must clamp low, got {second}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
