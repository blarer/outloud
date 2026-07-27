//! Recognizer backends.
//!
//! One module per engine. Everything implements [`crate::Recognizer`], so
//! the pipeline and callers never name a concrete backend outside of
//! construction/configuration code.

pub mod apple;
pub mod mock;
pub mod parakeet;
pub mod whisper_cpp;
