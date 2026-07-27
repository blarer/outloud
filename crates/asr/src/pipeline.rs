//! The two-stage pipeline: fast streaming partials + accurate finalizer.
//!
//! This is the seam the whole recognizer design hinges on (research §5,
//! backlog R-06). Two recognizers behind one facade:
//!
//! - The **streamer** gets every audio chunk as it arrives and paints ghost
//!   text. Its accuracy only has to be good enough to look right for a
//!   second or two.
//! - The **finalizer** gets the *complete* utterance at endpoint time and
//!   produces the text that is actually inserted. Its latency only has to
//!   beat ~200ms on a few seconds of audio, which batch models do easily.
//!
//! The pipeline itself implements [`Recognizer`], so a caller that only
//! wants one engine can pass the same object for both roles, and a caller
//! that wants two never learns which is which. Swapping Moonshine for
//! sherpa-onnx or SpeechTranscriber is a constructor argument.
//!
//! Arbitration rule (R-06 acceptance): the final transcript *replaces* the
//! last partial wholesale. No merging, no diffing. Merging partial and
//! final hypotheses is where duplicated/dropped words come from, so we
//! forbid it structurally: `finalize` never looks at the streamer's text.

use crate::{Partial, Recognizer, Transcript};

/// Latency budget for each stage, milliseconds. Written down in code (not
/// only docs) so instrumentation can compare measured numbers against the
/// budget and `doctor --timings` can flag regressions (R-08).
pub mod budget {
    /// Speech onset → first partial visible.
    pub const FIRST_PARTIAL_MS: u64 = 250;
    /// Between partial updates while speaking.
    pub const PARTIAL_CADENCE_MS: u64 = 150;
    /// VAD endpoint fires → final transcript ready.
    pub const FINALIZE_MS: u64 = 200;
    /// Endpoint hangover (silence before we decide the utterance is over).
    pub const HANGOVER_MS: u64 = 300;
}

/// Fast-partials + accurate-final, behind the same [`Recognizer`] trait.
pub struct TwoStagePipeline<S: Recognizer, F: Recognizer> {
    streamer: S,
    finalizer: F,
    /// The full utterance audio, buffered for the finalizer. The streamer
    /// consumes audio incrementally; the finalizer wants it whole, because
    /// batch models (Parakeet, whisper.cpp) are dramatically better and
    /// simpler on complete utterances than on chunks.
    utterance: Vec<f32>,
}

impl<S: Recognizer, F: Recognizer> TwoStagePipeline<S, F> {
    pub fn new(streamer: S, finalizer: F) -> Self {
        Self {
            streamer,
            finalizer,
            utterance: Vec::new(),
        }
    }
}

impl<S: Recognizer, F: Recognizer> Recognizer for TwoStagePipeline<S, F> {
    fn feed(&mut self, samples: &[f32]) -> Option<Partial> {
        self.utterance.extend_from_slice(samples);
        // Only the streamer sees incremental audio. The finalizer stays
        // cold until the endpoint, which is what keeps its accuracy path
        // simple and its memory footprint out of the hot loop.
        self.streamer.feed(samples)
    }

    fn finalize(&mut self) -> anyhow::Result<Transcript> {
        // Reset the streamer first so both stages are clean even if the
        // finalizer errors: the next utterance must never inherit state.
        let _ = self.streamer.finalize();
        let audio = std::mem::take(&mut self.utterance);
        // Feed the whole utterance to the finalizer in one call, then
        // finalize. Partials it might emit here are discarded on purpose:
        // by this point the user is waiting for committed text, not ghosts.
        let _ = self.finalizer.feed(&audio);
        self.finalizer.finalize()
    }

    fn name(&self) -> &'static str {
        "two-stage"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::mock::MockRecognizer;

    fn voiced(secs: f32) -> Vec<f32> {
        (0..(secs * 16_000.0) as usize)
            .map(|i| 0.3 * (i as f32 * 0.2).sin())
            .collect()
    }

    #[test]
    fn partials_stream_and_final_replaces() {
        let mut p = TwoStagePipeline::new(MockRecognizer::new(), MockRecognizer::new());
        let mut partials = Vec::new();
        for chunk in voiced(3.0).chunks(1600) {
            if let Some(part) = p.feed(chunk) {
                partials.push(part.text);
            }
        }
        assert!(!partials.is_empty(), "streamer must emit partials");
        let t = p.finalize().unwrap();
        // The finalizer saw the identical utterance, so with deterministic
        // recognizers its text equals the last partial. Same words, zero
        // duplication or loss: the R-06 acceptance shape.
        assert_eq!(t.text, *partials.last().unwrap());
    }

    #[test]
    fn utterances_are_independent() {
        let mut p = TwoStagePipeline::new(MockRecognizer::new(), MockRecognizer::new());
        p.feed(&voiced(2.0));
        let first = p.finalize().unwrap();
        p.feed(&voiced(2.0));
        let second = p.finalize().unwrap();
        assert_eq!(first.text, second.text, "state leaked across utterances");
    }

    #[test]
    fn empty_utterance_finalizes_empty() {
        let mut p = TwoStagePipeline::new(MockRecognizer::new(), MockRecognizer::new());
        let t = p.finalize().unwrap();
        assert_eq!(t.text, "");
    }
}
