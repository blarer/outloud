//! Deterministic recognizer for tests and CI.
//!
//! Why it exists: the pipeline's arbitration logic (partials replaced by
//! finals, utterance reset, ordering) must be testable without any model
//! download, GPU, or platform framework. This mock produces text purely
//! from audio *energy structure*, so tests control its output exactly by
//! constructing synthetic audio, and the pipeline under test is the real
//! pipeline, not a mock of itself.
//!
//! Behaviour: every second of audio that contains signal (RMS above a small
//! threshold per 100ms window) yields the next word from a fixed word list.
//! Silence yields nothing. Finalize returns all words seen, with fabricated
//! but monotonic word timings.

use crate::{Partial, Recognizer, Transcript, Word};

const SAMPLE_RATE: usize = 16_000;
/// Window in which we decide "was there signal": 100ms.
const WINDOW: usize = SAMPLE_RATE / 10;
/// Windows with signal needed to emit one word: 10 -> one word per second
/// of voiced audio. Coarse on purpose; tests count words, not phonemes.
const WINDOWS_PER_WORD: usize = 10;

const WORDS: &[&str] = &[
    "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "again", "tomorrow",
];

pub struct MockRecognizer {
    /// Voiced windows seen this utterance.
    voiced_windows: usize,
    /// Samples fed this utterance.
    samples_fed: usize,
    /// Carry buffer for partial windows across feed calls.
    tail: Vec<f32>,
    /// Words emitted so far.
    words: Vec<String>,
}

impl MockRecognizer {
    pub fn new() -> Self {
        Self {
            voiced_windows: 0,
            samples_fed: 0,
            tail: Vec::new(),
            words: Vec::new(),
        }
    }

    fn word_count(&self) -> usize {
        self.voiced_windows / WINDOWS_PER_WORD
    }
}

impl Default for MockRecognizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Recognizer for MockRecognizer {
    fn feed(&mut self, samples: &[f32]) -> Option<Partial> {
        self.samples_fed += samples.len();
        self.tail.extend_from_slice(samples);
        let full = self.tail.len() / WINDOW;
        let take = full * WINDOW;
        let drained: Vec<f32> = self.tail.drain(..take).collect();
        for w in drained.chunks_exact(WINDOW) {
            let rms = (w.iter().map(|s| s * s).sum::<f32>() / WINDOW as f32).sqrt();
            if rms > 0.01 {
                self.voiced_windows += 1;
            }
        }
        let target = self.word_count();
        if target > self.words.len() {
            while self.words.len() < target {
                self.words
                    .push(WORDS[self.words.len() % WORDS.len()].to_string());
            }
            Some(Partial {
                text: self.words.join(" "),
                audio_secs: self.samples_fed as f32 / SAMPLE_RATE as f32,
            })
        } else {
            None
        }
    }

    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        let audio_secs = self.samples_fed as f32 / SAMPLE_RATE as f32;
        let words: Vec<Word> = self
            .words
            .iter()
            .enumerate()
            .map(|(i, w)| Word {
                text: w.clone(),
                // Fabricated but monotonic: one word per second of voice.
                start_secs: i as f32,
                end_secs: i as f32 + 0.9,
            })
            .collect();
        let text = self.words.join(" ");
        // Reset for the next utterance, per the trait contract.
        *self = Self::new();
        Ok(Transcript {
            text,
            words,
            audio_secs,
        })
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voiced(secs: f32) -> Vec<f32> {
        (0..(secs * SAMPLE_RATE as f32) as usize)
            .map(|i| 0.3 * (i as f32 * 0.2).sin())
            .collect()
    }

    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; (secs * SAMPLE_RATE as f32) as usize]
    }

    #[test]
    fn silence_yields_no_partials_and_empty_transcript() {
        let mut r = MockRecognizer::new();
        assert!(r.feed(&silence(2.0)).is_none());
        let t = r.finalize().unwrap();
        assert_eq!(t.text, "");
        assert!(t.words.is_empty());
    }

    #[test]
    fn three_seconds_of_voice_yields_three_words() {
        let mut r = MockRecognizer::new();
        let mut last = None;
        for chunk in voiced(3.0).chunks(1234) {
            if let Some(p) = r.feed(chunk) {
                last = Some(p);
            }
        }
        assert_eq!(last.unwrap().text, "the quick brown");
        let t = r.finalize().unwrap();
        assert_eq!(t.text, "the quick brown");
        assert_eq!(t.words.len(), 3);
    }

    #[test]
    fn partials_grow_monotonically() {
        let mut r = MockRecognizer::new();
        let mut texts = Vec::new();
        for chunk in voiced(3.0).chunks(WINDOW) {
            if let Some(p) = r.feed(chunk) {
                texts.push(p.text);
            }
        }
        assert!(texts.len() >= 2);
        for pair in texts.windows(2) {
            assert!(pair[1].starts_with(&pair[0]));
        }
    }

    #[test]
    fn finalize_resets_for_next_utterance() {
        let mut r = MockRecognizer::new();
        r.feed(&voiced(1.5));
        let first = r.finalize().unwrap();
        assert_eq!(first.text, "the");
        r.feed(&voiced(1.5));
        let second = r.finalize().unwrap();
        assert_eq!(second.text, "the", "state must not leak across utterances");
    }
}
