//! The speech segmenter: VAD probabilities in, speech events out.
//!
//! This state machine decides *when an utterance starts and ends*, which is
//! where most of the perceived responsiveness of dictation lives. The R-02
//! budget is endpoint detection ≤350ms after speech end (300ms hangover +
//! ≤50ms compute); every constant here exists to hit that number without
//! chopping words.
//!
//! Design points:
//!
//! - **Onset debounce.** A single hot frame does not start an utterance;
//!   `min_speech_frames` consecutive speech frames do. This suppresses
//!   keyboard clicks and pops that even Silero occasionally scores high.
//! - **Pre-roll.** When speech starts we emit the frames from *before* the
//!   trigger too, because debouncing means the first syllable already
//!   happened. Without pre-roll, "hello" reliably becomes "ello", the
//!   classic endpointing bug in every first-draft dictation tool.
//! - **Hangover.** Speech does not end at the first silent frame; humans
//!   pause mid-sentence. `hangover` frames of continuous silence are
//!   required, defaulting to 300ms per the research recommendation.
//! - **Events, not buffers.** Callers get `SpeechStart`, `Partial` (audio to
//!   stream into the recognizer as it arrives), and `SpeechEnd` (with the
//!   full utterance for the finalizer pass). This is exactly the shape the
//!   two-stage recognizer in `crates/asr` consumes.

use crate::vad::VoiceDetector;
use crate::{FRAME_SAMPLES, SAMPLE_RATE};

/// What the segmenter tells its consumer.
#[derive(Debug, Clone, PartialEq)]
pub enum SpeechEvent {
    /// An utterance began. `audio` contains the pre-roll plus the frames
    /// that triggered the onset, so no leading syllable is lost.
    SpeechStart { audio: Vec<f32> },
    /// More speech audio inside an ongoing utterance. Feed it to the
    /// streaming recognizer immediately; do not wait for the end.
    Partial { audio: Vec<f32> },
    /// The utterance ended (hangover elapsed). `audio` is the complete
    /// utterance including pre-roll, for the accurate finalizer pass.
    /// `duration_secs` is of the speech itself, for diagnostics.
    SpeechEnd { audio: Vec<f32>, duration_secs: f32 },
}

/// Tunables. Defaults follow the research numbers (300ms hangover) and
/// common Silero deployment practice (0.5 threshold, ~90ms onset debounce).
#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    /// Probability at or above which a frame counts as speech.
    pub speech_threshold: f32,
    /// Consecutive speech frames required to declare an utterance start.
    pub min_speech_frames: usize,
    /// Consecutive silence frames required to declare the utterance over.
    /// 10 frames * 30ms = 300ms default hangover.
    pub hangover_frames: usize,
    /// Frames of audio retained before the onset trigger and prepended to
    /// the utterance.
    pub pre_roll_frames: usize,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            speech_threshold: 0.5,
            min_speech_frames: 3, // 90ms: rejects clicks, barely delays onset
            hangover_frames: 10,  // 300ms, per R-02 / research §5
            pre_roll_frames: 5,   // 150ms of context before the trigger
        }
    }
}

#[derive(Debug, PartialEq)]
enum State {
    /// Waiting for speech. Tracks how many consecutive speech frames seen.
    Silence { run: usize },
    /// Inside an utterance. Tracks consecutive silence frames seen.
    Speech { silence_run: usize },
}

/// Turns a stream of 30ms frames into [`SpeechEvent`]s.
pub struct SpeechSegmenter<V: VoiceDetector> {
    vad: V,
    config: SegmenterConfig,
    state: State,
    /// Rolling pre-roll of recent frames while silent, plus the onset run.
    pending: Vec<f32>,
    /// The complete current utterance, accumulated for `SpeechEnd`.
    utterance: Vec<f32>,
    /// Leftover samples smaller than one frame, carried between `push` calls.
    tail: Vec<f32>,
}

impl<V: VoiceDetector> SpeechSegmenter<V> {
    pub fn new(vad: V, config: SegmenterConfig) -> Self {
        Self {
            vad,
            config,
            state: State::Silence { run: 0 },
            pending: Vec::new(),
            utterance: Vec::new(),
            tail: Vec::new(),
        }
    }

    /// Feed captured samples (16kHz mono), receiving any events they cause.
    ///
    /// Accepts arbitrary chunk sizes; partial frames are carried internally.
    pub fn push(&mut self, samples: &[f32]) -> Vec<SpeechEvent> {
        let mut events = Vec::new();
        self.tail.extend_from_slice(samples);
        let full_frames = self.tail.len() / FRAME_SAMPLES;
        // Take the processable prefix; keep the remainder as the new tail.
        let take = full_frames * FRAME_SAMPLES;
        let frames: Vec<f32> = self.tail.drain(..take).collect();
        for frame in frames.chunks_exact(FRAME_SAMPLES) {
            self.step(frame, &mut events);
        }
        events
    }

    /// Force the current utterance closed, e.g. on hotkey release or device
    /// removal. A no-op when silent.
    pub fn flush(&mut self) -> Option<SpeechEvent> {
        match self.state {
            State::Silence { .. } => None,
            State::Speech { .. } => Some(self.end_utterance()),
        }
    }

    fn step(&mut self, frame: &[f32], events: &mut Vec<SpeechEvent>) {
        let is_speech = self.vad.speech_probability(frame) >= self.config.speech_threshold;
        match self.state {
            State::Silence { run } => {
                self.pending.extend_from_slice(frame);
                // Cap pending at pre-roll + onset debounce so silence does
                // not accumulate unbounded audio.
                let max_pending =
                    (self.config.pre_roll_frames + self.config.min_speech_frames) * FRAME_SAMPLES;
                if self.pending.len() > max_pending {
                    let excess = self.pending.len() - max_pending;
                    self.pending.drain(..excess);
                }
                if is_speech {
                    let run = run + 1;
                    if run >= self.config.min_speech_frames {
                        // Onset confirmed: everything pending (pre-roll +
                        // debounced frames) becomes the utterance head.
                        self.utterance = std::mem::take(&mut self.pending);
                        events.push(SpeechEvent::SpeechStart {
                            audio: self.utterance.clone(),
                        });
                        self.state = State::Speech { silence_run: 0 };
                    } else {
                        self.state = State::Silence { run };
                    }
                } else {
                    self.state = State::Silence { run: 0 };
                }
            }
            State::Speech { silence_run } => {
                self.utterance.extend_from_slice(frame);
                events.push(SpeechEvent::Partial {
                    audio: frame.to_vec(),
                });
                if is_speech {
                    self.state = State::Speech { silence_run: 0 };
                } else {
                    let silence_run = silence_run + 1;
                    if silence_run >= self.config.hangover_frames {
                        events.push(self.end_utterance());
                    } else {
                        self.state = State::Speech { silence_run };
                    }
                }
            }
        }
    }

    fn end_utterance(&mut self) -> SpeechEvent {
        // Reset VAD state between utterances: Silero carries RNN context
        // that would otherwise bias the next onset decision.
        self.vad.reset();
        self.state = State::Silence { run: 0 };
        self.pending.clear();
        let audio = std::mem::take(&mut self.utterance);
        let duration_secs = audio.len() as f32 / SAMPLE_RATE as f32;
        SpeechEvent::SpeechEnd {
            audio,
            duration_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::EnergyVad;

    /// Synthetic "speech": a 440Hz sine at amplitude 0.3, well above the
    /// energy VAD knee. Synthetic "silence": zeros.
    fn speech(frames: usize) -> Vec<f32> {
        (0..frames * FRAME_SAMPLES)
            .map(|i| {
                0.3 * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / SAMPLE_RATE as f32).sin()
            })
            .collect()
    }

    fn silence(frames: usize) -> Vec<f32> {
        vec![0.0; frames * FRAME_SAMPLES]
    }

    fn segmenter() -> SpeechSegmenter<EnergyVad> {
        SpeechSegmenter::new(EnergyVad::new(), SegmenterConfig::default())
    }

    #[test]
    fn pure_silence_emits_nothing() {
        let mut s = segmenter();
        assert!(s.push(&silence(100)).is_empty());
        assert!(s.flush().is_none());
    }

    #[test]
    fn utterance_produces_start_partials_end() {
        let mut s = segmenter();
        let mut events = Vec::new();
        events.extend(s.push(&silence(10)));
        events.extend(s.push(&speech(20)));
        events.extend(s.push(&silence(15))); // > 10-frame hangover
        let starts = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::SpeechStart { .. }))
            .count();
        let ends = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::SpeechEnd { .. }))
            .count();
        let partials = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::Partial { .. }))
            .count();
        assert_eq!(starts, 1, "events: {events:?}");
        assert_eq!(ends, 1);
        assert!(
            partials > 10,
            "expected a stream of partials, got {partials}"
        );
        // Order: start first, end last.
        assert!(matches!(
            events.first(),
            Some(SpeechEvent::SpeechStart { .. })
        ));
        assert!(matches!(events.last(), Some(SpeechEvent::SpeechEnd { .. })));
    }

    #[test]
    fn end_audio_contains_whole_utterance_with_pre_roll() {
        let mut s = segmenter();
        let mut end_audio = None;
        for e in s
            .push(&silence(10))
            .into_iter()
            .chain(s.push(&speech(20)))
            .chain(s.push(&silence(15)))
        {
            if let SpeechEvent::SpeechEnd { audio, .. } = e {
                end_audio = Some(audio);
            }
        }
        let audio = end_audio.expect("utterance must end");
        // 20 speech frames + 5 pre-roll frames minimum; hangover silence is
        // included too (harmless for recognizers, kept for simplicity).
        assert!(
            audio.len() >= 25 * FRAME_SAMPLES,
            "got {} samples",
            audio.len()
        );
    }

    #[test]
    fn single_hot_frame_is_debounced() {
        let mut s = segmenter();
        let mut events = Vec::new();
        events.extend(s.push(&silence(5)));
        events.extend(s.push(&speech(1))); // click: 1 frame < min_speech_frames
        events.extend(s.push(&silence(20)));
        assert!(
            events.is_empty(),
            "click must not start an utterance: {events:?}"
        );
    }

    #[test]
    fn short_pause_does_not_split_utterance() {
        let mut s = segmenter();
        let mut events = Vec::new();
        events.extend(s.push(&speech(10)));
        events.extend(s.push(&silence(5))); // 150ms pause < 300ms hangover
        events.extend(s.push(&speech(10)));
        events.extend(s.push(&silence(15)));
        let starts = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::SpeechStart { .. }))
            .count();
        assert_eq!(starts, 1, "mid-sentence pause split the utterance");
    }

    #[test]
    fn long_pause_splits_into_two_utterances() {
        let mut s = segmenter();
        let mut events = Vec::new();
        events.extend(s.push(&speech(10)));
        events.extend(s.push(&silence(15))); // > hangover: utterance 1 ends
        events.extend(s.push(&speech(10)));
        events.extend(s.push(&silence(15)));
        let starts = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::SpeechStart { .. }))
            .count();
        let ends = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::SpeechEnd { .. }))
            .count();
        assert_eq!((starts, ends), (2, 2));
    }

    #[test]
    fn hangover_timing_is_config_frames_after_speech_stops() {
        let mut s = segmenter();
        s.push(&speech(10));
        // 9 silence frames: still open.
        let e = s.push(&silence(9));
        assert!(!e.iter().any(|e| matches!(e, SpeechEvent::SpeechEnd { .. })));
        // 10th silence frame: closed. Endpoint = exactly hangover_frames.
        let e = s.push(&silence(1));
        assert!(e.iter().any(|e| matches!(e, SpeechEvent::SpeechEnd { .. })));
    }

    #[test]
    fn flush_closes_open_utterance() {
        let mut s = segmenter();
        s.push(&speech(10));
        match s.flush() {
            Some(SpeechEvent::SpeechEnd { audio, .. }) => assert!(!audio.is_empty()),
            other => panic!("expected SpeechEnd, got {other:?}"),
        }
        // Segmenter is reusable after flush.
        assert!(s.flush().is_none());
    }

    #[test]
    fn odd_chunk_sizes_are_equivalent_to_frame_aligned() {
        // Same audio pushed in 173-sample chunks vs whole must yield the
        // same event sequence, proving the tail-carry logic.
        let audio: Vec<f32> = silence(5)
            .into_iter()
            .chain(speech(15))
            .chain(silence(15))
            .collect();

        let mut whole = segmenter();
        let expected = whole.push(&audio);

        let mut chunked = segmenter();
        let mut got = Vec::new();
        for chunk in audio.chunks(173) {
            got.extend(chunked.push(chunk));
        }
        assert_eq!(expected, got);
    }
}
