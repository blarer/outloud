//! Local LLM layer for freeform edit-by-voice transformations.
//!
//! `edit-intent` handles the closed set of commands (replace/delete/append/
//! recase) deterministically in microseconds. Everything else comes back as
//! `EditIntent::Freeform { instruction }`, and this crate is what makes those
//! instructions ("tighten this up", "make it more formal", "turn this into
//! bullet points") actually work, entirely on-device.
//!
//! Two properties matter more here than raw capability, and both are enforced
//! structurally rather than hoped for:
//!
//! 1. **The model must not hallucinate edits nobody asked for.** Raw model
//!    output never reaches the caller directly: it passes through
//!    [`sanitize::sanitize`] (strip fences, prefixes, thinking blocks) and
//!    then [`guardrail::check`] (refusal/echo detection, length-ratio bound,
//!    diff-size bound). A rejected output is an error, not a paste.
//! 2. **The slow path must not block the fast path.** This crate is only ever
//!    invoked for `Freeform`; the deterministic parser never waits on it.
//!    Within the slow path, [`Transformer::transform_streaming`] delivers
//!    tokens progressively so the preview panel (docs/ux/03-edit-by-voice.md)
//!    shows text appearing rather than a multi-second blank.
//!
//! Per docs/ux/03, freeform output is **preview-only**: [`transform`] returns
//! a [`PreviewedEdit`] that the caller must show to the user before writing
//! anything into their document. There is deliberately no "apply" API here.

pub mod guardrail;
pub mod models;
pub mod prompt;
pub mod sanitize;

#[cfg(feature = "llama")]
pub mod llama_backend;

use guardrail::{GuardrailConfig, Rejection};

/// Why a transformation could not produce a usable result.
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    /// The backend itself failed (model not loaded, inference error, ...).
    #[error("backend error: {0}")]
    Backend(String),
    /// The model produced output, but the guardrails rejected it. The raw
    /// output is carried for diagnostics, never for display in the document.
    #[error("output rejected: {rejection}")]
    Rejected {
        rejection: Rejection,
        raw_output: String,
    },
    /// After sanitation nothing was left. Distinct from `Rejected` because
    /// the fix (retry, maybe with different sampling) differs from the fix
    /// for a refusal (rephrase the instruction).
    #[error("model produced empty output")]
    Empty,
}

/// A text transformation backend: given the original text and a freeform
/// instruction, produce the transformed text.
///
/// Implementations return the *raw* model output. Sanitation and guardrails
/// are applied by [`transform`], not by backends, so every backend gets the
/// same safety behaviour for free and cannot forget it.
pub trait Transformer {
    /// Transform `original` per `instruction`, delivering raw output
    /// incrementally through `on_token` as it is generated. The full raw
    /// output is also returned. Chunk boundaries are backend-defined
    /// (llama.cpp yields per-token; the mock yields word-ish chunks).
    ///
    /// Streaming exists purely for preview progressiveness: callers must
    /// treat streamed chunks as *unvetted* (guardrails run on the complete
    /// output) and render them only inside the preview panel.
    fn transform_streaming(
        &mut self,
        original: &str,
        instruction: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<String, TransformError>;

    /// Convenience: transform without observing intermediate tokens.
    fn transform(&mut self, original: &str, instruction: &str) -> Result<String, TransformError> {
        self.transform_streaming(original, instruction, &mut |_| {})
    }
}

/// A guardrail-approved transformation, ready to *preview*.
///
/// Note what is absent: nothing here writes to the document. Per
/// docs/ux/03-edit-by-voice.md, freeform edits always preview first, so this
/// type is the input to the diff-preview panel, and applying is the caller's
/// explicit, user-confirmed act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewedEdit {
    /// The text the transformation was computed against. Carried so the
    /// preview can detect that the field changed underneath it (stale edit)
    /// and so the diff can be rendered without re-reading the field.
    pub original: String,
    /// Sanitized, guardrail-approved replacement text.
    pub transformed: String,
    /// The instruction that produced it, for display in the preview header.
    pub instruction: String,
}

/// Run the full freeform path: backend -> sanitize -> guardrails -> preview.
///
/// This is the one entry point callers should use. `on_token` receives raw
/// streamed chunks for progressive preview rendering; the returned
/// [`PreviewedEdit`] is the vetted final result and may differ from the
/// concatenation of streamed chunks (fences and prefixes get stripped).
pub fn transform(
    backend: &mut dyn Transformer,
    original: &str,
    instruction: &str,
    config: &GuardrailConfig,
    on_token: &mut dyn FnMut(&str),
) -> Result<PreviewedEdit, TransformError> {
    let raw = backend.transform_streaming(original, instruction, on_token)?;
    let cleaned = sanitize::sanitize(&raw, instruction);
    if cleaned.is_empty() {
        return Err(TransformError::Empty);
    }
    if let Some(rejection) = guardrail::check(original, &cleaned, instruction, config) {
        return Err(TransformError::Rejected {
            rejection,
            raw_output: raw,
        });
    }
    Ok(PreviewedEdit {
        original: original.to_string(),
        transformed: cleaned,
        instruction: instruction.to_string(),
    })
}

/// Deterministic backend for tests and CI. Never touches a model, never
/// touches the network, and streams in word-sized chunks so streaming
/// consumers are exercised too.
///
/// The canned behaviours cover the shapes real models produce, including the
/// misbehaviours the sanitizer and guardrails exist for, so the full pipeline
/// is testable without a download.
pub struct MockTransformer {
    /// Fixed output to return regardless of input. `None` means "echo the
    /// original with a trivial tightening" so happy-path tests read well.
    pub canned: Option<String>,
}

impl MockTransformer {
    /// Happy-path mock: applies a trivial deterministic "tighten" (collapses
    /// whitespace) so output plausibly relates to input.
    pub fn new() -> Self {
        Self { canned: None }
    }

    /// Mock that always emits `output`, for driving sanitizer/guardrail paths.
    pub fn with_output(output: impl Into<String>) -> Self {
        Self {
            canned: Some(output.into()),
        }
    }
}

impl Default for MockTransformer {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for MockTransformer {
    fn transform_streaming(
        &mut self,
        original: &str,
        _instruction: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<String, TransformError> {
        let out = match &self.canned {
            Some(c) => c.clone(),
            None => original.split_whitespace().collect::<Vec<_>>().join(" "),
        };
        // Stream word-ish chunks so tests observe more than one callback.
        let mut rest = out.as_str();
        while let Some(idx) = rest.find(' ') {
            on_token(&rest[..=idx]);
            rest = &rest[idx + 1..];
        }
        if !rest.is_empty() {
            on_token(rest);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GuardrailConfig {
        GuardrailConfig::default()
    }

    #[test]
    fn full_path_happy_case_previews() {
        let mut mock = MockTransformer::with_output("The deploy must happen today.");
        let original = "It is really quite important that we should try to make \
                        sure the deploy happens today.";
        let mut streamed = String::new();
        let edit = transform(&mut mock, original, "tighten this up", &cfg(), &mut |t| {
            streamed.push_str(t)
        })
        .unwrap();
        assert_eq!(edit.transformed, "The deploy must happen today.");
        assert_eq!(edit.original, original);
        // Streaming delivered the whole raw output progressively.
        assert_eq!(streamed, "The deploy must happen today.");
    }

    #[test]
    fn streaming_yields_multiple_chunks() {
        let mut mock = MockTransformer::with_output("one two three");
        let mut chunks = Vec::new();
        transform(&mut mock, "x y z", "noop", &cfg(), &mut |t| {
            chunks.push(t.to_string())
        })
        .unwrap();
        assert!(chunks.len() >= 3, "expected word chunks, got {chunks:?}");
    }

    #[test]
    fn fenced_output_is_sanitized_before_preview() {
        let mut mock = MockTransformer::with_output("```\nShort and clear.\n```");
        let edit = transform(
            &mut mock,
            "This sentence is short and it is also clear.",
            "tighten this up",
            &cfg(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(edit.transformed, "Short and clear.");
    }

    #[test]
    fn refusal_is_rejected_not_previewed() {
        let mut mock =
            MockTransformer::with_output("I'm sorry, but I can't help with rewriting that text.");
        let err =
            transform(&mut mock, "some text here", "tighten", &cfg(), &mut |_| {}).unwrap_err();
        assert!(matches!(
            err,
            TransformError::Rejected {
                rejection: Rejection::Refusal,
                ..
            }
        ));
    }

    #[test]
    fn runaway_length_is_rejected() {
        let long = "word ".repeat(200);
        let mut mock = MockTransformer::with_output(long);
        let err = transform(
            &mut mock,
            "short input sentence",
            "tighten this up",
            &cfg(),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(
            err,
            TransformError::Rejected {
                rejection: Rejection::LengthRatio { .. },
                ..
            }
        ));
    }

    #[test]
    fn empty_after_sanitation_is_empty_error() {
        let mut mock = MockTransformer::with_output("```\n```");
        let err = transform(&mut mock, "text", "tighten", &cfg(), &mut |_| {}).unwrap_err();
        assert!(matches!(err, TransformError::Empty));
    }

    #[test]
    fn default_mock_collapses_whitespace() {
        let mut mock = MockTransformer::new();
        let out = mock.transform("a   b\n c", "anything").unwrap();
        assert_eq!(out, "a b c");
    }
}
