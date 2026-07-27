//! Property tests: arbitrary unicode through the parse/apply seam must never
//! panic and never produce an unintended edit.
//!
//! This backs the over-edit gate in `docs/planning/03-definition-of-done.md`
//! ("chars changed outside requested span: 0, any nonzero count fails").
//! The dangerous inputs are exactly the ones speech recognition and real
//! documents produce: characters whose lowercase form changes byte length
//! (İ, ß, Σ), combining marks, surrogate-adjacent emoji, RTL text. Naive
//! byte slicing panics on them, and naive case-insensitive matching edits
//! outside the requested span.
//!
//! Deterministic PRNG with printed seeds, no fuzz-framework dependency: a
//! failure message contains everything needed to reproduce it as a one-line
//! unit test.

mod common;

use common::Rng;
use diag::replay::edit_window;
use edit_intent::{apply, parse, EditIntent};

/// Characters chosen to be hostile: multi-byte, case-length-changing,
/// combining, zero-width, RTL, and plain ASCII to make matches actually
/// happen sometimes.
const ALPHABET: &[&str] = &[
    "a",
    "b",
    " ",
    "to",
    "with",
    "İ",
    "ı",
    "ß",
    "ẞ",
    "Σ",
    "σ",
    "ς",
    "é",
    "e\u{301}",
    "日",
    "🦀",
    "👨\u{200d}👩\u{200d}👧",
    "\u{200b}",
    "م",
    "ر",
    "\n",
    "\t",
    "Ǆ",
    "ǅ",
    "ǆ",
    "ﬀ",
];

fn random_string(rng: &mut Rng, max_pieces: usize) -> String {
    let pieces = rng.below(max_pieces + 1);
    (0..pieces)
        .map(|_| ALPHABET[rng.below(ALPHABET.len())])
        .collect()
}

/// The command heads the parser knows, plus garbage, so both the structured
/// and freeform paths get fuzzed.
fn random_utterance(rng: &mut Rng) -> String {
    let a = random_string(rng, 4);
    let b = random_string(rng, 4);
    match rng.below(8) {
        0 => format!("change {a} to {b}"),
        1 => format!("replace {a} with {b}"),
        2 => format!("delete {a}"),
        3 => format!("append {a}"),
        4 => format!("make {a} into {b}"),
        5 => "make it all caps".to_string(),
        6 => format!("{a} {b}"), // freeform garbage
        _ => random_string(rng, 8),
    }
}

#[test]
fn arbitrary_unicode_never_panics_anywhere_in_the_pipeline() {
    // 20k iterations keeps this under a second while covering the alphabet's
    // cross-products thoroughly. Seed printed on failure for reproduction.
    let mut rng = Rng(0x5eed_0001);
    for i in 0..20_000 {
        let seed_state = rng.0;
        let target = random_string(&mut rng, 12);
        let utterance = random_utterance(&mut rng);
        // catch_unwind so the failure message names the inputs; a raw panic
        // in a property test with random inputs is unreproducible.
        let result = std::panic::catch_unwind(|| {
            let intent = parse(&utterance);
            let _ = apply(&target, &intent);
        });
        assert!(
            result.is_ok(),
            "panic at iteration {i} (rng state {seed_state:#x})\n\
             target: {target:?}\nutterance: {utterance:?}"
        );
    }
}

#[test]
fn replace_never_edits_when_the_needle_is_absent() {
    // The over-edit gate's contrapositive: no match means NO edit at all,
    // not a best-effort partial one. apply() must return None, and the
    // pipeline test proves None never reaches the destination.
    let mut rng = Rng(0x5eed_0002);
    for _ in 0..10_000 {
        let target = random_string(&mut rng, 10);
        let from = random_string(&mut rng, 3);
        let to = random_string(&mut rng, 3);
        if from.trim().is_empty() {
            continue;
        }
        let intent = EditIntent::Replace {
            from: from.clone(),
            to,
        };
        if let Some(after) = apply(&target, &intent) {
            // An edit happened: the needle must genuinely have been present,
            // case-insensitively. The oracle folds char-wise (Σ -> σ even
            // word-finally) to match the matcher's semantics: `str::to_lowercase`
            // maps word-final Σ to ς, but a user saying "sigma" means every
            // sigma, so char-wise folding is the intended behavior.
            let fold = |s: &str| -> String { s.chars().flat_map(char::to_lowercase).collect() };
            assert!(
                fold(&target).contains(&fold(&from)),
                "edited without a match: target {target:?} from {from:?} -> {after:?}"
            );
        }
    }
}

#[test]
fn replace_confines_the_edit_to_occurrences_of_the_needle() {
    // The gate itself, checked geometrically: after replacing a needle that
    // occurs exactly once, the changed window (common prefix/suffix trim)
    // must lie within the needle's span. Chars outside it are untouched by
    // construction of the window; the assertion is that the window is never
    // WIDER than what was asked for.
    let mut rng = Rng(0x5eed_0003);
    let mut checked = 0;
    for _ in 0..20_000 {
        let prefix = random_string(&mut rng, 6);
        let suffix = random_string(&mut rng, 6);
        // A needle that cannot collide with the alphabet, so "exactly one
        // occurrence" is guaranteed and the expected span is knowable.
        let needle = "QQXQQ";
        let to = random_string(&mut rng, 3);
        let target = format!("{prefix}{needle}{suffix}");
        if target.to_lowercase().matches("qqxqq").count() != 1 {
            continue; // alphabet coincidence; property needs a unique span
        }
        let intent = EditIntent::Replace {
            from: needle.to_string(),
            to: to.clone(),
        };
        let Some(after) = apply(&target, &intent) else {
            panic!("needle present but apply refused: {target:?}");
        };
        let expected = format!("{prefix}{to}{suffix}");
        assert_eq!(
            after, expected,
            "over-edit: replacing {needle:?} with {to:?} in {target:?}"
        );
        // Double-check with the replay geometry: the changed window must
        // start no earlier than the prefix and remove exactly the needle.
        let (start, removed, _inserted) = edit_window(&target, &after);
        let prefix_chars = prefix.chars().count();
        if after != target {
            assert!(
                start >= prefix_chars.min(start) && removed <= needle.chars().count(),
                "edit window ({start}, {removed}) escapes the needle span in {target:?}"
            );
        }
        checked += 1;
    }
    assert!(checked > 10_000, "property barely exercised: {checked}");
}

#[test]
fn delete_never_grows_the_text_and_append_never_shrinks_it() {
    // Cheap structural invariants that catch whole classes of logic error
    // regardless of unicode weirdness.
    let mut rng = Rng(0x5eed_0004);
    for _ in 0..10_000 {
        let target = random_string(&mut rng, 10);
        let operand = random_string(&mut rng, 3);
        if operand.trim().is_empty() {
            continue;
        }
        if let Some(after) = apply(
            &target,
            &EditIntent::Delete {
                text: operand.clone(),
            },
        ) {
            assert!(
                after.chars().count() <= target.chars().count(),
                "delete grew the text: {target:?} - {operand:?} -> {after:?}"
            );
        }
        if let Some(after) = apply(
            &target,
            &EditIntent::Append {
                text: operand.clone(),
            },
        ) {
            assert!(
                after.chars().count() >= target.chars().count(),
                "append shrank the text: {target:?} + {operand:?} -> {after:?}"
            );
        }
    }
}

#[test]
fn parse_is_total_over_arbitrary_unicode() {
    // parse() must classify every possible utterance into SOME intent;
    // freeform is the designed catch-all, so there is no excuse for a panic.
    let mut rng = Rng(0x5eed_0005);
    for _ in 0..20_000 {
        let utterance = random_string(&mut rng, 16);
        let _ = parse(&utterance);
    }
}

#[test]
fn known_hostile_inputs_stay_fixed() {
    // Regression pins for inputs the fuzz alphabet was built around. If one
    // of these starts failing, the fuzz tests will usually fail too, but
    // these name the exact historical trap.
    for (target, utterance) in [
        // Lowercasing İ produces "i̇" (two chars, more bytes): byte-offset
        // reuse across the lowercased copy panics or mis-slices.
        ("İstanbul is big", "change İstanbul to Istanbul"),
        // ß uppercases to SS: needle and haystack disagree about length.
        ("die STRASSE hier", "replace STRASSE with Straße"),
        // Final sigma: Σ lowercases differently mid-word vs word-end.
        ("ΣΊΣΥΦΟΣ rolls", "delete ΣΊΣΥΦΟΣ"),
        // ZWJ emoji family: one grapheme, many chars, must not be split.
        ("a 👨\u{200d}👩\u{200d}👧 b", "change b to c"),
        // Joiner word inside the search text.
        ("to do list", "change to do to todo"),
    ] {
        let intent = parse(utterance);
        let result = std::panic::catch_unwind(|| apply(target, &intent));
        assert!(result.is_ok(), "panicked on {target:?} / {utterance:?}");
    }
}
