//! The two delivery paths now agree about what a freeform phrase means.
//!
//! They did not always. `payload_for` (the pure decision function the tier
//! ladder uses, compiled on every platform so the decision is testable on
//! macOS CI) reported EVERY unparsed phrase as
//! `Outcome::FreeformUnsupported`, while the macOS `deliver` path inserted
//! EVERY unparsed phrase as dictation:
//!
//! ```ignore
//! if let EditIntent::Freeform { .. } = &intent {
//!     return insert_with_fallback(text);   // the COMMAND, as text
//! }
//! ```
//!
//! Both defaults are wrong for half the traffic, and that is why they
//! diverged: neither could tell "the user dictated while a stale selection
//! happened to exist" from "the user issued an instruction about the
//! selection". Saying "tighten this up" with a sentence selected typed the
//! words "Tighten this up." over the sentence, reported as success.
//!
//! `outloud::freeform::classify` draws the line, and both paths now consult
//! it, so this file asserts the SAME contract for both halves:
//!
//! - a recognisable rewrite request is refused, and nothing is written;
//! - ordinary prose is written verbatim, even with a selection live.
//!
//! Only the pure half is exercised here, so this touches no transport,
//! needs no accessibility grant, and cannot type anywhere.
//!
//! See docs/investigations/edit-intent.md.

use outloud::freeform::{classify, FreeformDisposition};
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

/// Prose a user would dictate. None of it parses as a command either, so
/// the ONLY thing separating these two lists is the classifier.
const PLAIN_PROSE: &[&str] = &[
    "the meeting is at three tomorrow afternoon",
    "we should tell them soon",
    "fix the login bug and add tests",
    "this is just a normal sentence",
];

const SELECTED: &str = "It is really quite important that we ship today.";

#[test]
fn payload_for_reports_freeform_rather_than_writing_it() {
    for instruction in FREEFORM_INSTRUCTIONS {
        let mode = Mode::Edit {
            selected: SELECTED.into(),
        };
        match payload_for(&mode, instruction) {
            Err(Outcome::FreeformUnsupported { instruction: got }) => {
                assert_eq!(got, *instruction);
            }
            Ok(payload) => panic!(
                "{instruction:?} produced a writable payload {payload:?}; \
                 a rewrite request must never reach a transport"
            ),
            Err(other) => panic!("{instruction:?} gave {other:?}, expected FreeformUnsupported"),
        }
    }
}

/// The counter-case the macOS branch existed to protect, and the reason
/// the fix could not be "always report freeform": a plain dictated
/// sentence spoken while something is selected must still be written.
#[test]
fn plain_prose_with_a_selection_is_written_not_refused() {
    for prose in PLAIN_PROSE {
        let mode = Mode::Edit {
            selected: "some prose".into(),
        };
        match payload_for(&mode, prose) {
            Ok(payload) => assert_eq!(
                payload, *prose,
                "dictation must be written verbatim, not transformed"
            ),
            Err(other) => panic!(
                "{prose:?} was refused ({other:?}); refusing ordinary dictation \
                 presents as \"the app stopped transcribing\""
            ),
        }
    }
}

/// The divergence itself is what regressed a user, so the agreement is
/// asserted directly rather than inferred from the two tests above: for
/// every phrase, `classify`'s verdict and `payload_for`'s outcome must be
/// the same decision.
#[test]
fn both_halves_of_the_delivery_path_reach_the_same_verdict() {
    for phrase in FREEFORM_INSTRUCTIONS.iter().chain(PLAIN_PROSE) {
        let mode = Mode::Edit {
            selected: SELECTED.into(),
        };
        let refused_by_payload = matches!(
            payload_for(&mode, phrase),
            Err(Outcome::FreeformUnsupported { .. })
        );
        let refused_by_classifier = matches!(
            classify(phrase, SELECTED),
            FreeformDisposition::RewriteRequest { .. }
        );
        assert_eq!(
            refused_by_payload, refused_by_classifier,
            "{phrase:?}: the delivery paths disagree, which is the bug class \
             that let one path insert what the other refused"
        );
    }
}

/// The documented escape hatch (`docs/ux/03`): a false refusal must cost
/// the user exactly one retry, so there has to be a way to write words
/// that look like an instruction.
#[test]
fn the_type_prefix_costs_one_retry_and_recovers_any_false_refusal() {
    let mode = Mode::Edit {
        selected: SELECTED.into(),
    };
    assert!(matches!(
        payload_for(&mode, "tighten this up"),
        Err(Outcome::FreeformUnsupported { .. })
    ));
    assert_eq!(
        payload_for(&mode, "type: tighten this up").unwrap(),
        "tighten this up",
        "the escape hatch must write the literal words, prefix removed"
    );
}
