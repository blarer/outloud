//! `OUTLOUD_NO_INJECT=1` reports the edit decision without performing it.
//!
//! The guard exists so tests and benchmarks can run the pipeline without
//! typing into whatever application happens to be focused (commit d495b47,
//! "stop tests typing into apps"). It works, and it is the right default.
//!
//! It used to return from `deliver` BEFORE `edit_intent::parse` was ever
//! called, so under the guard the daemon reported only what the recognizer
//! heard. This file previously pinned that behaviour and recorded the cost:
//! "the safe measurement mode cannot exercise, and therefore cannot
//! regression-test, any edit-by-voice behaviour."
//!
//! That cost came due. The undo ring shipped complete, tested, and wired to
//! nothing, because the only way to observe which branch an edit command
//! took was to speak into a live window and watch. Every automated route
//! ended here, at an early return.
//!
//! So the guard now runs the ROUTING (which is pure: no accessibility tree,
//! no clipboard, no keystrokes) and reports the chosen route alongside the
//! transcript. What it still refuses to do is the part that touches the
//! user's machine. Deciding and doing are separate, and only the doing is
//! unsafe to run in a test.
//!
//! This lives in its own integration binary rather than in `inject.rs`'s unit
//! tests because it mutates a process-global environment variable, and cargo
//! runs unit tests in one process with threads. Setting the variable there
//! races the sibling tests that call `deliver` expecting the real path.
//! (In-process, `crate::testenv::no_inject` binds the variable to a lock so
//! that race cannot happen; this binary is isolated and needs no lock.)
//!
//! See docs/investigations/edit-intent.md.

use outloud::inject::{deliver, Mode, Outcome};

#[test]
fn no_inject_reports_the_route_but_writes_nothing() {
    // Sole test in this binary, so this is safe: nothing else reads the var.
    std::env::set_var("OUTLOUD_NO_INJECT", "1");

    // A well-formed edit command against a selection that contains its
    // search text, so it parses to `Replace` and would apply cleanly.
    let mode = Mode::Edit {
        selected: "some prose".into(),
    };
    let out = deliver(&mode, "change some to other");

    match out {
        Outcome::Suppressed { text } => {
            // The route is named, so an automated run can tell "this was
            // understood as a rewrite" from "this fell through to no-match"
            // without a human watching a window.
            assert!(
                text.contains("[route: rewrite]"),
                "the dry run must name the route it chose, got {text:?}"
            );
            // But the rewrite itself was never performed: the transcript is
            // echoed, not the rewritten text ("other prose"). Nothing was
            // computed against the user's selection and nothing was typed.
            assert!(
                text.starts_with("change some to other"),
                "the transcript must be echoed verbatim, got {text:?}"
            );
            assert!(
                !text.contains("other prose"),
                "no edit may be applied under the guard, got {text:?}"
            );
        }
        other => panic!("expected Suppressed, got {other:?}"),
    }
}

/// The route reported must be the route that would really be taken.
///
/// A dry run that names a route nobody would follow is worse than no dry
/// run: it invites exactly the false confidence this whole file is about.
#[test]
fn the_reported_route_matches_the_real_decision() {
    std::env::set_var("OUTLOUD_NO_INJECT", "1");

    for (phrase, selected, expected) in [
        ("scratch that", "some prose", "undo"),
        ("change some to other", "some prose", "rewrite"),
        ("change zebra to lion", "some prose", "no-match"),
        (
            "tighten this up",
            "a long selected paragraph",
            "unsupported",
        ),
    ] {
        let mode = Mode::Edit {
            selected: selected.into(),
        };
        // The routing function's own answer, and the one the dry run prints,
        // must agree. They are the same call, and this pins that they stay
        // the same call.
        let route = outloud::inject::route_edit(phrase, selected);
        assert_eq!(route.label(), expected, "{phrase:?} routed unexpectedly");

        match deliver(&mode, phrase) {
            Outcome::Suppressed { text } => assert!(
                text.contains(&format!("[route: {expected}]")),
                "{phrase:?} should report {expected}, got {text:?}"
            ),
            other => panic!("expected Suppressed, got {other:?}"),
        }
    }
}
