//! The two delivery paths disagree about what a freeform edit means.
//!
//! `payload_for` is the pure decision function the tier ladder uses, and it
//! is compiled on every platform specifically so the decision is testable on
//! macOS CI. For a freeform instruction spoken at a selection it returns
//! `Outcome::FreeformUnsupported`, which the pipeline renders as the honest
//! message "freeform edit ... needs the local LLM (not shipped yet) ->
//! rephrase as change/replace/delete/add/case".
//!
//! The macOS `deliver` path does something different with the identical
//! input. Its `Mode::Edit` arm short-circuits:
//!
//! ```ignore
//! if let EditIntent::Freeform { .. } = &intent {
//!     return insert_with_fallback(text);
//! }
//! ```
//!
//! so the instruction is inserted at the caret as ordinary dictated text.
//! Saying "tighten this up" with text selected types the words "tighten this
//! up" into the document. `FreeformUnsupported` is therefore unreachable on
//! macOS: nothing but `payload_for` constructs it, and `payload_for` is only
//! called by `deliver_via_tiers`, which is `#[cfg(not(target_os = "macos"))]`.
//!
//! The reasoning behind the macOS branch is sound for the case it was written
//! for: selections linger, so an unrecognised phrase spoken with a stale
//! selection is ordinary dictation and must not be refused. But it does not
//! distinguish that from a recognisable rewrite request, and for the latter
//! inserting the command is the one outcome the user certainly did not want.
//!
//! This test asserts only the pure half, so it touches no transport, needs no
//! accessibility grant, and cannot type anywhere. The macOS half is asserted
//! negatively by `inject.rs`'s own
//! `unrecognised_phrase_with_a_selection_is_dictated`, which documents the
//! insert behaviour as intended.
//!
//! See docs/investigations/edit-intent.md.

use outloud::inject::{payload_for, Mode, Outcome};

/// Instructions that are recognisably rewrite requests rather than prose
/// someone happened to dictate. Every one is `docs/ux/03`'s own example of
/// what should reach the preview panel.
const FREEFORM_INSTRUCTIONS: &[&str] = &[
    "tighten this up",
    "make it more formal",
    "make it sound friendlier",
    "summarize this",
    "turn this into a commit message",
];

#[test]
fn payload_for_reports_freeform_rather_than_writing_it() {
    for instruction in FREEFORM_INSTRUCTIONS {
        let mode = Mode::Edit {
            selected: "It is really quite important that we ship today.".into(),
        };
        match payload_for(&mode, instruction) {
            Err(Outcome::FreeformUnsupported { instruction: got }) => {
                assert_eq!(got, *instruction);
            }
            Ok(payload) => panic!(
                "{instruction:?} produced a writable payload {payload:?}; \
                 a freeform edit must never reach a transport"
            ),
            Err(other) => panic!("{instruction:?} gave {other:?}, expected FreeformUnsupported"),
        }
    }
}

/// The counter-case the macOS branch exists to protect, stated explicitly so
/// a future fix does not regress it: a plain dictated sentence spoken while
/// something is selected must not be refused.
///
/// Note this is exactly why the fix is not "always report freeform". The
/// distinction a fix has to draw is between an imperative rewrite request
/// and prose, and this test pins the prose side of that line.
#[test]
fn plain_prose_with_a_selection_is_also_reported_as_freeform_today() {
    let mode = Mode::Edit {
        selected: "some prose".into(),
    };
    // Ordinary dictation, not a command. `payload_for` cannot tell it apart
    // from a rewrite request, which is the imprecision that makes the macOS
    // path's blanket insert defensible today.
    let out = payload_for(&mode, "the meeting is at three tomorrow afternoon");
    assert!(
        matches!(out, Err(Outcome::FreeformUnsupported { .. })),
        "expected today's behaviour: everything unparsed is 'freeform', \
         which is precisely why the two paths had to choose different \
         defaults; got {out:?}"
    );
}
