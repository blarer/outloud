//! Shared transcript types.

/// An in-flight hypothesis. Replaces any previous partial wholesale.
#[derive(Debug, Clone, PartialEq)]
pub struct Partial {
    /// Best current guess at the utterance text so far.
    pub text: String,
    /// Audio consumed so far, in seconds, so the UI can align ghost text
    /// with what the user actually said.
    pub audio_secs: f32,
}

/// A word with timing, when the backend provides it (Parakeet and
/// SpeechTranscriber do, whisper.cpp approximates, the mock fabricates).
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub start_secs: f32,
    pub end_secs: f32,
}

/// The committed result of one utterance.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    /// Per-word timing when available; empty otherwise. Optional-by-empty
    /// rather than `Option<Vec<..>>` because every consumer treats "no
    /// timings" and "zero words" identically.
    pub words: Vec<Word>,
    /// Total audio duration recognized, in seconds.
    pub audio_secs: f32,
}

impl Transcript {
    /// An empty transcript, for zero-length utterances.
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            words: Vec::new(),
            audio_secs: 0.0,
        }
    }
}
