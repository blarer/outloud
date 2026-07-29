//! `OUTLOUD_NO_INJECT=1` short-circuits before any edit decision is made.
//!
//! The guard exists so tests and benchmarks can run the pipeline without
//! typing into whatever application happens to be focused (commit d495b47,
//! "stop tests typing into apps"). It works, and it is the right default.
//!
//! What it also does, which is not obvious from the flag's name or its
//! documentation in `docs/configuration.md`, is return from `deliver` BEFORE
//! `edit_intent::parse` is ever called. So under the guard the daemon reports
//! only what the recognizer heard: dictate-vs-edit dispatch, intent parsing,
//! and outcome selection are all skipped.
//!
//! Two consequences, both worth pinning:
//!
//! 1. The safe measurement mode cannot exercise, and therefore cannot
//!    regression-test, any edit-by-voice behaviour.
//! 2. Every `deliver`-based assertion in `inject.rs`'s unit tests becomes
//!    vacuous when the guard is set, because `Suppressed` trivially satisfies
//!    "not `FreeformUnsupported`" and "not `Wrote`". The decision logic is
//!    genuinely covered by the `payload_for` tests, which are pure.
//!
//! This lives in its own integration binary rather than in `inject.rs`'s unit
//! tests because it mutates a process-global environment variable, and cargo
//! runs unit tests in one process with threads. Setting the variable there
//! races the sibling tests that call `deliver` expecting the real path.
//!
//! See docs/investigations/edit-intent.md.

use outloud::inject::{deliver, Mode, Outcome};

#[test]
fn no_inject_returns_before_the_edit_decision() {
    // Sole test in this binary, so this is safe: nothing else reads the var.
    std::env::set_var("OUTLOUD_NO_INJECT", "1");

    // A well-formed edit command against a selection that contains its
    // search text. With the guard off this parses to `Replace` and applies
    // cleanly, so `Suppressed` can only mean the guard returned first.
    let mode = Mode::Edit {
        selected: "some prose".into(),
    };
    let out = deliver(&mode, "change some to other");

    match out {
        Outcome::Suppressed { text } => {
            // The transcript is echoed verbatim: not the rewritten text
            // ("other prose"), which confirms no edit was ever computed.
            assert_eq!(
                text, "change some to other",
                "Suppressed should carry the raw transcript, not an edit result"
            );
        }
        other => panic!("expected Suppressed, got {other:?}"),
    }
}
