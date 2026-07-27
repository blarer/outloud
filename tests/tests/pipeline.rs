//! The full edit-by-voice loop across crate boundaries:
//! read (text-target) -> parse (edit-intent) -> apply (edit-intent) ->
//! write (text-target), asserting on the text that actually arrived at the
//! destination, not on return codes.
//!
//! The controllable target is `StdioFilterTarget` over in-memory buffers: it
//! is the one real transport whose destination the test can fully observe,
//! because the destination *is* a byte buffer we hold. Everything the loop
//! does to a real app it does here, minus the OS.

mod common;

use edit_intent::{apply, parse, EditIntent};
use text_target::targets::headless::{frame, parse_frame, StdioFilterTarget};
use text_target::{TargetError, TextTarget};

/// Run one utterance through the whole pipeline against a destination whose
/// current contents are `initial`. Returns what the destination would hold
/// afterwards, decoded from the actual protocol bytes the target emitted.
fn run_pipeline(initial: &str, utterance: &str) -> Result<String, String> {
    // The "app": pushes its buffer, receives a REPLACE.
    let input = frame("BUFFER", Some(initial));
    let mut wire: Vec<u8> = Vec::new();
    let mut target = StdioFilterTarget::new(input.as_bytes(), &mut wire);

    // read
    target.pump().map_err(|e| e.to_string())?;
    let snapshot = target.read().map_err(|e| e.to_string())?;

    // parse + apply
    let intent = parse(utterance);
    let new_text = match apply(&snapshot.text, &intent) {
        Some(t) => t,
        None => return Err(format!("no deterministic application for {intent:?}")),
    };

    // write
    target.replace(&new_text).map_err(|e| e.to_string())?;
    drop(target);

    // Assert on the wire, not on our own bookkeeping: decode what the
    // destination was actually told to do.
    let written = String::from_utf8(wire).expect("protocol output is UTF-8");
    let (verb, payload) = parse_frame(&written).map_err(|e| e.to_string())?;
    assert_eq!(verb, "REPLACE", "pipeline must replace, not insert");
    Ok(payload.expect("REPLACE carries a payload"))
}

#[test]
fn replace_command_lands_verbatim_at_the_destination() {
    let after = run_pipeline(
        "the quick brown fox jumps over the lazy dog",
        "change quick to slow",
    )
    .unwrap();
    assert_eq!(after, "the slow brown fox jumps over the lazy dog");
}

#[test]
fn delete_command_cleans_whitespace_at_the_destination() {
    let after = run_pipeline("a very long day", "delete very").unwrap();
    assert_eq!(after, "a long day");
}

#[test]
fn append_lands_with_correct_spacing() {
    let after = run_pipeline("hello", "append world").unwrap();
    assert_eq!(after, "hello world");
}

#[test]
fn recase_transforms_the_whole_field() {
    let after = run_pipeline("hello world", "make it all caps").unwrap();
    assert_eq!(after, "HELLO WORLD");
}

#[test]
fn multiline_field_content_survives_the_protocol() {
    // The daemon protocol is line-framed; a field containing newlines is the
    // classic way to break line-framed protocols. base64 framing must carry
    // it intact end to end.
    let after = run_pipeline("first line\nsecond line\nthird", "change second to 2nd").unwrap();
    assert_eq!(after, "first line\n2nd line\nthird");
}

#[test]
fn unmatched_edit_never_reaches_the_destination() {
    // "did not match" must stop the pipeline BEFORE the write. Writing the
    // unchanged text back would still clobber the destination's cursor and
    // undo state for zero benefit.
    let err = run_pipeline("nothing relevant here", "change absent to present").unwrap_err();
    assert!(err.contains("no deterministic application"), "{err}");
}

#[test]
fn freeform_utterance_stops_at_the_model_boundary() {
    // Freeform intents need a language model; the deterministic pipeline
    // must refuse rather than guess, because a wrong guess is an over-edit.
    let err = run_pipeline("some text", "make this sound more professional").unwrap_err();
    assert!(err.contains("Freeform"), "{err}");
}

#[test]
fn read_before_any_buffer_is_an_explicit_error_not_empty_text() {
    // Seam bug shape: if an unprimed read returned "" instead of an error,
    // the pipeline would happily "edit" an empty field and replace the
    // user's real text with the transformed empty string.
    let mut out: Vec<u8> = Vec::new();
    let mut target = StdioFilterTarget::new(&b""[..], &mut out);
    match target.read() {
        Err(TargetError::NotReadable(_)) => {}
        other => panic!("expected NotReadable, got {other:?}"),
    }
}

#[test]
fn pipeline_roundtrip_preserves_untouched_regions_exactly() {
    // The over-edit gate at the seam: everything outside the requested span
    // must be byte-identical after the trip through parse/apply/protocol.
    let initial = "prefix UNIQUE suffix with unicode: héllo wörld 日本語";
    let after = run_pipeline(initial, "change UNIQUE to CHANGED").unwrap();
    assert_eq!(
        after,
        "prefix CHANGED suffix with unicode: héllo wörld 日本語"
    );
    let (start, removed, inserted) = diag::replay::edit_window(initial, &after);
    assert_eq!(
        (start, removed, inserted),
        (7, 6, 7),
        "edit must be confined to the UNIQUE span"
    );
}

#[test]
fn intent_shapes_recorded_for_replay_match_what_ran() {
    // The seam between the live pipeline and diag's recorder: the record's
    // intent shape must describe the intent that actually executed, or a
    // replayed session debugs the wrong command.
    let intent = parse("change quick to slow");
    let (kind, from, to) = match &intent {
        EditIntent::Replace { from, to } => ("replace", from.as_str(), to.as_str()),
        other => panic!("unexpected intent {other:?}"),
    };
    let mut rec = diag::replay::SessionRecord::new();
    rec.record_intent(kind, from, to);
    let parsed = diag::replay::SessionRecord::parse(&rec.serialize()).unwrap();
    let i = parsed.intent.unwrap();
    assert_eq!(
        (i.kind.as_str(), i.from_chars, i.to_chars),
        ("replace", 5, 4)
    );
}
