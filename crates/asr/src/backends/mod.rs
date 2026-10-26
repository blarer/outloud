//! Recognizer backends.
//!
//! One module per engine. Everything implements [`crate::Recognizer`], so
//! the pipeline and callers never name a concrete backend outside of
//! construction/configuration code.

pub mod apple;
pub mod mock;
pub mod parakeet;
pub mod whisper_cpp;

// NVIDIA Nemotron ASR through the nemo-speech CLI. Not feature-gated,
// because unlike whisper_cpp it links nothing: it shells out, so a build
// without the CLI present compiles fine and fails at construction with a
// message naming the fix.
pub mod nemo;
