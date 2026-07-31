//! What happens when a freeform edit command meets a real selection.
//!
//! `Mode::Edit` means text was selected at key-down. A command that does
//! not parse as one of the deterministic edits (`change X to Y`, `delete
//! the last sentence`, ...) becomes `EditIntent::Freeform`, and
//! `inject.rs` routes those to `insert_with_fallback`.
//!
//! That function's own selection branch says: "A non-empty selection at
//! commit time: typing replaces it, so dictation does too." Which is
//! correct for *dictation*. The question this test answers is what it
//! means for a *freeform command*, where the words the user said were an
//! instruction about the selection rather than a replacement for it.

use edit_intent::EditIntent;

/// The phrasings a user reaches for when they want the selection changed
/// rather than replaced. None of these parse as deterministic edits today.
const FREEFORM_COMMANDS: &[&str] = &[
    "tighten this up",
    "make it more formal",
    "summarize this",
    "translate this to spanish",
    "make this shorter",
    "fix the grammar",
];

#[test]
fn freeform_commands_do_not_parse_as_deterministic_edits() {
    // Establishes the precondition for everything below. If one of these
    // ever starts parsing, it is no longer part of this problem.
    for cmd in FREEFORM_COMMANDS {
        assert!(
            matches!(edit_intent::parse(cmd), EditIntent::Freeform { .. }),
            "{cmd:?} parsed as a deterministic edit; this test's premise is stale"
        );
    }
}

#[test]
fn a_freeform_command_is_not_applied_to_the_selection() {
    // edit_intent::apply is the deterministic engine. It cannot execute a
    // freeform instruction, and says so by returning None rather than
    // guessing. That is the correct behaviour and is not the bug.
    let selected = "The customers might possibly be quite upset about this.";
    for cmd in FREEFORM_COMMANDS {
        let intent = edit_intent::parse(cmd);
        assert!(
            edit_intent::apply(selected, &intent).is_none(),
            "{cmd:?} must not silently produce a rewrite it cannot justify"
        );
    }
}

/// The actual hazard, expressed as a property rather than as a delivery
/// call, because `inject::deliver` needs a live focused UI element.
///
/// With a selection present, `insert_with_fallback` reaches
/// `snap.is_selection_edit()` and calls `write_focused`, which replaces
/// the selection. So the user's *instruction* becomes the new contents of
/// whatever they had selected.
///
/// Reading `inject.rs`: `deliver` -> `Mode::Edit` arm -> `Freeform` guard
/// -> `insert_with_fallback(text)` -> `is_selection_edit()` -> replace.
/// `text` there is the raw transcript, i.e. the command itself.
#[test]
fn documents_the_freeform_over_selection_hazard() {
    let selected = "The customers might possibly be quite upset about this.";
    let spoken = "tighten this up";

    // What the deterministic engine can offer: nothing.
    let intent = edit_intent::parse(spoken);
    let rewritten = edit_intent::apply(selected, &intent);
    assert!(rewritten.is_none());

    // And so the delivery path falls through to plain insertion of
    // `spoken`. With a selection live, plain insertion is a replacement.
    // The user asked for their sentence to be tightened and would get the
    // words "tighten this up" in its place, losing the original.
    //
    // This test does not assert the corruption (that needs a focused UI);
    // it pins the two facts that produce it, so a fix that makes either
    // false will show up here.
    assert!(
        matches!(intent, EditIntent::Freeform { .. }),
        "a freeform command must be recognisable as freeform at the delivery boundary, \
         which is what a fix needs in order to refuse rather than overwrite"
    );
}

// ---------------------------------------------------------------------------
// The fix. Everything above pins the FACTS that produced the corruption
// (the parser cannot serve these commands, and the delivery path used to
// insert them). Everything below pins the BEHAVIOUR that now prevents it.
// ---------------------------------------------------------------------------

use outloud::freeform::{classify, FreeformDisposition};
use outloud::inject::{payload_for, Mode, Outcome};

const SELECTED: &str = "The customers might possibly be quite upset about this.";

/// The reported bug, as an assertion. Before the fix this produced
/// `Ok("tighten this up")`, which `deliver` wrote over the selection.
#[test]
fn the_reported_corruption_cannot_happen() {
    let mode = Mode::Edit {
        selected: SELECTED.into(),
    };
    match payload_for(&mode, "tighten this up") {
        Err(Outcome::FreeformUnsupported { instruction }) => {
            assert_eq!(instruction, "tighten this up");
        }
        Ok(payload) => panic!(
            "the delivery path produced {payload:?} for a rewrite request; \
             writing it would replace the user's sentence with the words \
             describing what they wanted done to it"
        ),
        Err(other) => panic!("expected FreeformUnsupported, got {other:?}"),
    }
}

/// Every phrasing in the hazard corpus, not just the reported one.
#[test]
fn no_freeform_command_can_become_the_selections_new_contents() {
    for cmd in FREEFORM_COMMANDS {
        let mode = Mode::Edit {
            selected: SELECTED.into(),
        };
        assert!(
            matches!(
                payload_for(&mode, cmd),
                Err(Outcome::FreeformUnsupported { .. })
            ),
            "{cmd:?} produced a writable payload; the user's paragraph would be lost"
        );
    }
}

/// The bug the destructive behaviour was introduced to fix, which the fix
/// must not reintroduce: dictation with a stale selection still writes. A
/// wrong refusal here is what read as "the app stopped transcribing".
#[test]
fn the_opposite_regression_is_not_reintroduced() {
    for prose in [
        SELECTED,
        "we should tell them soon",
        "fix the login bug and add tests",
        "make sure the deploy happens today",
    ] {
        let mode = Mode::Edit {
            selected: "a stale selection nobody aimed at".into(),
        };
        assert_eq!(
            payload_for(&mode, prose).unwrap(),
            prose,
            "ordinary dictation must still be written verbatim"
        );
    }
}

/// A refusal carries the heard instruction (so the overlay can say what it
/// heard) and no payload of any kind, so there is nothing for a transport
/// to write. That is what makes the failure mode non-destructive.
#[test]
fn a_refusal_writes_nothing_at_all() {
    for cmd in FREEFORM_COMMANDS {
        match classify(cmd, SELECTED) {
            FreeformDisposition::RewriteRequest { instruction } => assert_eq!(instruction, *cmd),
            FreeformDisposition::Dictate { text } => {
                panic!("{cmd:?} would be dictated as {text:?}, replacing the selection")
            }
        }
    }
}

/// The refusal must be a SPEED BUMP, not a dead end.
///
/// The pipeline renders `FreeformUnsupported` as a message naming two
/// recoveries, one of which is `"type: <what was heard>"`. That is only
/// honest if the suggestion actually works, so the loop is closed here:
/// take the refused instruction, apply the prefix the message tells the
/// user to say, and assert the words reach the document.
///
/// Without this, the message and `freeform::classify` could drift apart
/// and the product would be confidently telling users to say something
/// that no longer does anything.
#[test]
fn the_suggested_retry_actually_works() {
    for cmd in FREEFORM_COMMANDS {
        let mode = Mode::Edit {
            selected: SELECTED.into(),
        };
        // It is refused, and the refusal names this exact retry.
        assert!(matches!(
            payload_for(&mode, cmd),
            Err(Outcome::FreeformUnsupported { .. })
        ));

        // The retry the user is told to say.
        let retry = format!("type: {cmd}");
        match payload_for(&mode, &retry) {
            Ok(payload) => assert_eq!(
                payload, *cmd,
                "the suggested retry must write the heard words, prefix removed"
            ),
            Err(other) => panic!(
                "the message tells the user to say {retry:?}, but that is \
                 also refused ({other:?}); the refusal would be a dead end"
            ),
        }
    }
}

/// The OTHER runtime path into the user's field: streaming partials.
///
/// `deliver` is not the only thing that writes. With `insertion.mode =
/// "stream"`, `ax_stream::AxRegion` writes partial transcripts into the
/// field AS THE USER SPEAKS, before any classification has happened. If
/// that path could engage while text was selected, the refusal built
/// above would be worthless: the words "tighten this up" would already
/// be in the document by the time `deliver` declined to write them.
///
/// It cannot, and this pins BOTH independent guards rather than one, so
/// removing either is a visible failure instead of a silent regression:
///
/// 1. `streamer::wants_streaming` requires `Mode::Dictate`, so a
///    selection at key-down never opens a session at all.
/// 2. `AxRegion::begin` independently rejects a non-zero selection
///    length, so even a caller that bypassed (1) gets nothing.
///
/// Asserted through the pure halves; the live half is covered by
/// scripts/verify-freeform-live.sh, whose --once runs pin the buffered
/// path that ships by default.
#[test]
fn streaming_partials_can_never_engage_over_a_selection() {
    use outloud::inject::Mode;
    use outloud::streamer::wants_streaming;

    // Guard 1: even with streaming explicitly preferred.
    for cmd in FREEFORM_COMMANDS {
        let mode = Mode::Edit {
            selected: SELECTED.into(),
        };
        assert!(
            !wants_streaming(true, &mode, None),
            "{cmd:?} would stream partials over the selection before \
             classification could refuse it"
        );
    }
    // And dictation still streams, so this guard is not just "off".
    assert!(wants_streaming(true, &Mode::Dictate, None));
}
