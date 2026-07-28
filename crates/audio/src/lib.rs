//! Microphone capture and speech segmentation for the recognizer pipeline.
//!
//! M0 proved the OS-integration half of edit-by-voice costs ~47ms of an 800ms
//! budget. Everything else is the recognizer, and the recognizer starts here:
//! this crate owns the path from the microphone to clean 16kHz mono f32
//! speech segments, annotated with start/end events, that `crates/asr` can
//! feed to any backend.
//!
//! The layering is deliberate:
//!
//! - [`ring`]: a lock-free-enough SPSC ring buffer sized in seconds, because
//!   the cpal callback runs on a realtime audio thread that must never block
//!   or allocate. Audio is *dropped oldest-first* under overrun rather than
//!   stalling capture, because a recognizer that is 2s behind is useless for
//!   dictation anyway.
//! - [`resample`]: everything downstream speaks 16kHz mono f32 (the lingua
//!   franca of every ASR model surveyed in the research), but hardware often
//!   only offers 44.1/48kHz. Linear resampling is sufficient: ASR models are
//!   trained on far worse channel distortion than -30dB interpolation images.
//! - [`vad`]: a `VoiceDetector` trait with an energy-based implementation
//!   that is always available and a Silero ONNX implementation behind the
//!   `silero` feature. The trait exists so the segmenter's state machine is
//!   testable with synthetic audio and swappable to semantic VAD later.
//! - [`segment`]: the [`segment::SpeechSegmenter`] state machine that turns
//!   per-frame speech probabilities into `SpeechStart` / `Partial` /
//!   `SpeechEnd` events with configurable hangover. This is where the
//!   endpointing latency (R-02's ≤350ms budget) is decided.
//! - [`capture`]: cpal device enumeration, stream setup, and hotplug
//!   recovery. Kept at the edge because it is the only part that needs real
//!   hardware, so everything else stays testable in CI.

// The capture backend lives in a backend-suffixed file so a future
// platform-native backend (e.g. WASAPI on Windows) can sit beside it and be
// selected by `cfg` without churning every `audio::capture::*` call site.
// cpal covers CoreAudio/WASAPI/ALSA today, so it is the only backend.
#[path = "capture_cpal.rs"]
pub mod capture;
pub mod resample;
pub mod ring;
pub mod segment;
pub mod vad;

/// The sample rate every downstream consumer expects. All surveyed ASR
/// models (Whisper, Parakeet, Moonshine, Silero VAD, SpeechTranscriber's
/// preferred input) take 16kHz mono, so we normalize once at the edge.
pub const SAMPLE_RATE: u32 = 16_000;

/// Frame size the VAD and segmenter operate on: 30ms at 16kHz. Chosen
/// because Silero accepts 30ms frames, and 30ms granularity keeps hangover
/// timing resolution well under the endpointing budget.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize * 30) / 1000;
