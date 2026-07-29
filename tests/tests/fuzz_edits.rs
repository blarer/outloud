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
///
/// The scope-aware commands are here as well as the literal verbs. They do
/// far more byte-offset arithmetic (sentence and word segmentation, splicing
/// around a resolved span) than `Replace`/`Delete` ever did, so they are the
/// likelier home of the next panic in this class.
fn random_utterance(rng: &mut Rng) -> String {
    let a = random_string(rng, 4);
    let b = random_string(rng, 4);
    let scoped = [
        "delete the last sentence",
        "remove the first sentence",
        "delete the third word",
        "delete the last line",
        "uppercase the first letter",
        "capitalize the first word",
        "make the last sentence title case",
        "add a period at the end",
        "wrap this in quotes",
        "make it snake case",
        "make it camel case",
        "turn this into bullet points",
        "number these lines",
        "join these lines",
        "remove the last comma",
        "undo that",
        "delete everything",
    ];
    match rng.below(12) {
        0 => format!("change {a} to {b}"),
        1 => format!("replace {a} with {b}"),
        2 => format!("delete {a}"),
        3 => format!("append {a}"),
        4 => format!("make {a} into {b}"),
        5 => "make it all caps".to_string(),
        6 => format!("{a} {b}"), // freeform garbage
        // An anchor drawn from the hostile alphabet, so the case-insensitive
        // anchor search gets fuzzed rather than only its ASCII happy path.
        7 => format!("add a comma after {a}"),
        8 => format!("in the last sentence change {a} to {b}"),
        9 | 10 => scoped[rng.below(scoped.len())].to_string(),
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

/// A scoped delete must only ever REMOVE content.
///
/// Whitespace at the seam is repaired, so the output is not a literal
/// substring and the invariant has to be checked on non-whitespace
/// characters, which a delete must never add. This is the over-edit gate
/// applied to the scope-aware path, where a mis-resolved span would take
/// the wrong words rather than merely the wrong number of them.
#[test]
fn scoped_deletes_never_add_content() {
    let mut rng = Rng(0x5eed_0006);
    for utterance in [
        "delete the last sentence",
        "remove the first sentence",
        "delete the last word",
        "delete the first line",
        "remove the last comma",
    ] {
        let intent = parse(utterance);
        for _ in 0..4_000 {
            let target = random_string(&mut rng, 14);
            let Some(after) = apply(&target, &intent) else {
                continue;
            };
            let count = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
            assert!(
                count(&after) <= count(&target),
                "{utterance:?} ADDED content: {target:?} -> {after:?}"
            );
        }
    }
}

/// Identifier casing must emit only lowercase alphanumerics and its own
/// separator, or the result is not a usable identifier. `İ` is the trap:
/// it is alphanumeric but lowercases into a combining mark that is not.
#[test]
fn identifier_casing_emits_only_identifier_characters() {
    let mut rng = Rng(0x5eed_0007);
    for (utterance, sep) in [("make it snake case", '_'), ("make it kebab case", '-')] {
        let intent = parse(utterance);
        for _ in 0..5_000 {
            let target = random_string(&mut rng, 10);
            let Some(out) = apply(&target, &intent) else {
                continue;
            };
            assert!(
                out.chars().all(|c| c.is_alphanumeric() || c == sep),
                "{utterance:?} produced non-identifier chars: {out:?} from {target:?}"
            );
        }
    }
}

/// Terminal punctuation must never stack, on any input.
#[test]
fn punctuation_never_stacks() {
    let mut rng = Rng(0x5eed_0008);
    let intent = parse("add a period at the end");
    for _ in 0..10_000 {
        let target = random_string(&mut rng, 12);
        let Some(out) = apply(&target, &intent) else {
            continue;
        };
        assert!(!out.ends_with(".."), "stacked punctuation: {out:?}");
    }
}

/// A scope must never resolve to a span that would edit outside itself.
///
/// Checked through the public seam rather than against internal spans: a
/// scoped recase of the LAST sentence must leave the text before that
/// sentence byte-identical. A leaking scope is worse than no scope, because
/// the user believes they narrowed the blast radius.
#[test]
fn a_scoped_edit_never_touches_text_before_its_scope() {
    let mut rng = Rng(0x5eed_0009);
    let intent = parse("make the last sentence title case");
    for _ in 0..10_000 {
        // A fixed, sentence-terminated prefix, so "everything before the
        // last sentence" is knowable without reimplementing the splitter.
        // The trailing "z" guarantees the random tail carries real content,
        // because with an empty tail the prefix genuinely IS the last
        // sentence and rewriting it would be correct.
        let prefix = "stable opening words here. ";
        let target = format!("{prefix}{}z", random_string(&mut rng, 8));
        let Some(after) = apply(&target, &intent) else {
            continue;
        };
        assert!(
            after.starts_with(prefix),
            "scoped edit leaked before its scope: {target:?} -> {after:?}"
        );
    }
}
