//! Apple SpeechTranscriber backend (macOS 26+), via a Swift helper process.
//!
//! ## Why this backend
//!
//! On macOS 26 the OS ships an on-device speech model with streaming
//! volatile results, punctuation, and OS-managed model assets: the user
//! downloads nothing, we store nothing, and RAM for the model is charged to
//! the system, not the app (research §1.9). That makes it the default
//! backend on new Macs, with Parakeet/whisper.cpp as the cross-platform and
//! older-macOS fallbacks.
//!
//! ## Why a helper process, not FFI
//!
//! `SpeechAnalyzer` is a Swift-concurrency API (actors, async streams). A
//! child process speaking newline-delimited JSON over pipes costs one fork
//! and ~60ms of startup (measured), is debuggable with a shell one-liner,
//! and crash-isolates the OS speech stack from the app. The helper source
//! lives at `crates/asr/helper/transcriber.swift`; build it with
//! `swiftc -O transcriber.swift -o aqua-speech-helper`.
//!
//! ## Measured on this machine (M-series, macOS 26.5, 2026-07)
//!
//! Fed `say`-generated audio at real-time pace, 200ms chunks:
//!
//! - Helper ready (process spawn to analyzer live): **~60-220ms**.
//! - 2.5s utterance, full transcription after EOF: **~560ms wall** including
//!   spawn; text exact ("The quick brown fox jumps over the lazy dog.").
//! - 12s multi-sentence input: finals arrive per sentence; the tail final
//!   lands **~0.9s after EOF**. Well inside the 200ms/5s finalizer budget
//!   when amortized, comfortably fast as a finalizer.
//! - Caveat measured honestly: volatile partials arrived in *bursts at
//!   sentence boundaries* (~4.3s in) rather than word-by-word on synthetic
//!   TTS audio. Whether real microphone audio with natural pauses behaves
//!   better is an open question for M1; until then treat this backend as an
//!   excellent zero-install *finalizer* and keep the streamer slot for
//!   Moonshine/Zipformer.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::{Partial, Recognizer, Transcript};

/// One line of helper output. Field names match the Swift side.
#[derive(Debug, serde::Deserialize)]
struct HelperEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    message: String,
}

/// Locate the helper binary: explicit override, then next to the current
/// executable (release layout), then the in-repo build (dev layout).
fn find_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AQUA_SPEECH_HELPER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("aqua-speech-helper");
            if p.exists() {
                return Some(p);
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("helper")
        .join("aqua-speech-helper");
    dev.exists().then_some(dev)
}

pub struct AppleRecognizer {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<HelperEvent>,
    samples_fed: usize,
    last_partial: Option<String>,
}

impl AppleRecognizer {
    /// Spawn the helper and wait for its `ready` event.
    ///
    /// Errors if the helper binary is missing (not macOS 26, or not built)
    /// so callers can fall back to another backend explicitly rather than
    /// silently recognizing nothing.
    pub fn new() -> anyhow::Result<Self> {
        let helper = find_helper().ok_or_else(|| {
            anyhow::anyhow!(
                "aqua-speech-helper not found; build it with \
                 `swiftc -O crates/asr/helper/transcriber.swift -o aqua-speech-helper` \
                 or set AQUA_SPEECH_HELPER"
            )
        })?;
        let mut child = Command::new(&helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");

        // Reader thread: helper lines -> channel. A thread (not polling)
        // because partials must surface the moment the helper prints them.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("apple-asr-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(ev) = serde_json::from_str::<HelperEvent>(&line) {
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                }
            })?;

        // Block for readiness: first use may include an OS model download,
        // and pretending to accept audio before the analyzer exists would
        // silently drop the start of the first utterance.
        let ready_timeout = std::time::Duration::from_secs(120);
        match rx.recv_timeout(ready_timeout) {
            Ok(ev) if ev.kind == "ready" => {}
            Ok(ev) if ev.kind == "error" => {
                anyhow::bail!("speech helper failed to start: {}", ev.message)
            }
            Ok(ev) => anyhow::bail!("unexpected first helper event: {:?}", ev.kind),
            Err(_) => anyhow::bail!("speech helper did not become ready in {ready_timeout:?}"),
        }

        Ok(Self {
            child,
            stdin,
            events: rx,
            samples_fed: 0,
            last_partial: None,
        })
    }

    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(ev) => match ev.kind.as_str() {
                    // Finals also update the running text: the helper emits
                    // cumulative text on both event kinds.
                    "partial" | "final" => self.last_partial = Some(ev.text),
                    _ => {}
                },
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }
}

impl Recognizer for AppleRecognizer {
    fn feed(&mut self, samples: &[f32]) -> Option<Partial> {
        if let Some(stdin) = self.stdin.as_mut() {
            // f32 -> little-endian bytes; the helper's wire format.
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                // Helper died mid-utterance; finalize will surface it.
                self.stdin = None;
            }
        }
        self.samples_fed += samples.len();
        let before = self.last_partial.clone();
        self.drain_events();
        if self.last_partial != before {
            self.last_partial.clone().map(|text| Partial {
                text,
                audio_secs: self.samples_fed as f32 / 16_000.0,
            })
        } else {
            None
        }
    }

    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        // Closing stdin signals end-of-utterance; the helper finalizes and
        // prints the remaining events then `done`.
        drop(self.stdin.take());
        let mut final_text = self.last_partial.clone().unwrap_or_default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(ev) => match ev.kind.as_str() {
                    "partial" | "final" => final_text = ev.text,
                    "done" => break,
                    "error" => anyhow::bail!("speech helper error: {}", ev.message),
                    _ => {}
                },
                Err(_) => anyhow::bail!("speech helper did not finish in time"),
            }
        }
        let audio_secs = self.samples_fed as f32 / 16_000.0;
        let _ = self.child.wait();
        Ok(Transcript {
            text: final_text,
            // SpeechTranscriber has word timing via attributes; the helper
            // does not forward it yet. Empty is honest (see types.rs).
            words: Vec::new(),
            audio_secs,
        })
    }

    fn name(&self) -> &'static str {
        "apple-speechtranscriber"
    }
}

impl Drop for AppleRecognizer {
    fn drop(&mut self) {
        // Never leave orphan helpers around: kill on drop if still running.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end against the real OS speech stack. Ignored by default:
    /// requires macOS 26+, the built helper, and the OS model asset. Run
    /// with `cargo test -p asr -- --ignored` on a suitable machine.
    #[test]
    #[ignore = "requires macOS 26 SpeechTranscriber and built helper"]
    fn transcribes_synthesized_speech() {
        // Synthesize a known sentence with `say`, convert to 16k f32.
        let dir = std::env::temp_dir().join("aqua-apple-asr-test");
        std::fs::create_dir_all(&dir).unwrap();
        let aiff = dir.join("t.aiff");
        let caf = dir.join("t.caf");
        let run = |cmd: &str, args: &[&str]| {
            let ok = std::process::Command::new(cmd)
                .args(args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "{cmd} failed");
        };
        run(
            "say",
            &["-o", aiff.to_str().unwrap(), "hello world this is a test"],
        );
        run(
            "afconvert",
            &[
                "-f",
                "caff",
                "-d",
                "LEF32@16000",
                "-c",
                "1",
                aiff.to_str().unwrap(),
                caf.to_str().unwrap(),
            ],
        );
        // Extract the raw data chunk from the CAF container.
        let bytes = std::fs::read(&caf).unwrap();
        let idx = bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("caf data chunk");
        let payload = &bytes[idx + 16..];
        let samples: Vec<f32> = payload
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        let mut r = AppleRecognizer::new().expect("helper must start");
        for chunk in samples.chunks(3200) {
            r.feed(chunk);
        }
        let t = r.finalize().unwrap();
        // SpeechTranscriber punctuates ("Hello, world, this is a test."),
        // so compare on letters only.
        let letters: String = t
            .text
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect();
        assert!(
            letters.contains("helloworldthisisatest"),
            "got: {:?}",
            t.text
        );
    }
}
