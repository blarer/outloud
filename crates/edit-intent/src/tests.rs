//! Tests for the parse/apply seam.
//!
//! The bar, set by the existing suite and by
//! `docs/planning/03-definition-of-done.md`: a command this crate claims to
//! handle is tested against the **exact** resulting string, never merely
//! against "it parsed". A parser that recognises a command and then applies
//! it wrongly is worse than one that refuses, because the user sees a
//! confident result and has to spot the damage unaided.
//!
//! The corpus in [`corpus`] is the coverage claim itself, and it carries the
//! commands expected to FAIL as well as the ones expected to work.

use super::*;

// ---------------------------------------------------------------------
// original grammar: unchanged behaviour
// ---------------------------------------------------------------------

#[test]
fn parses_change_to() {
    assert_eq!(
        parse("change hello to goodbye"),
        EditIntent::Replace {
            from: "hello".into(),
            to: "goodbye".into()
        }
    );
}

#[test]
fn parses_replace_with() {
    assert_eq!(
        parse("replace foo with bar"),
        EditIntent::Replace {
            from: "foo".into(),
            to: "bar".into()
        }
    );
}

#[test]
fn splits_on_last_joiner_so_search_text_survives() {
    // The word "to" appears inside the search text; a naive first-match
    // split would produce from="" and lose the command.
    assert_eq!(
        parse("change to do to todo"),
        EditIntent::Replace {
            from: "to do".into(),
            to: "todo".into()
        }
    );
}

#[test]
fn parses_literal_delete_and_append() {
    assert_eq!(
        parse("delete really"),
        EditIntent::Delete {
            text: "really".into()
        }
    );
    assert_eq!(
        parse("append and thanks"),
        EditIntent::Append {
            text: "and thanks".into()
        }
    );
}

#[test]
fn parses_casing() {
    assert_eq!(parse("make it all caps"), EditIntent::Recase(Case::Upper));
    assert_eq!(parse("title case please"), EditIntent::Recase(Case::Title));
}

#[test]
fn unknown_phrasing_becomes_freeform() {
    let intent = parse("tighten this up and make it sound friendlier");
    assert!(matches!(intent, EditIntent::Freeform { .. }));
}

#[test]
fn apply_replaces_ignoring_case() {
    let intent = EditIntent::Replace {
        from: "hello".into(),
        to: "goodbye".into(),
    };
    assert_eq!(
        apply("Hello world, hello again", &intent).unwrap(),
        "goodbye world, goodbye again"
    );
}

#[test]
fn apply_reports_no_match() {
    let intent = EditIntent::Replace {
        from: "absent".into(),
        to: "x".into(),
    };
    assert!(apply("nothing here", &intent).is_none());
}

#[test]
fn apply_delete_cleans_up_whitespace() {
    let intent = EditIntent::Delete {
        text: "very ".into(),
    };
    assert_eq!(apply("a very long day", &intent).unwrap(), "a long day");
}

#[test]
fn apply_append_spaces_correctly() {
    let intent = EditIntent::Append {
        text: "world".into(),
    };
    assert_eq!(apply("hello", &intent).unwrap(), "hello world");
    assert_eq!(apply("hello ", &intent).unwrap(), "hello world");
    assert_eq!(apply("", &intent).unwrap(), "world");
}

#[test]
fn freeform_has_no_deterministic_application() {
    let intent = EditIntent::Freeform {
        instruction: "make it nicer".into(),
    };
    assert!(apply("text", &intent).is_none());
}

#[test]
fn non_ascii_input_does_not_panic() {
    // Turkish, German, and Greek all have characters whose lowercase form
    // differs in byte length, which is exactly where naive byte slicing
    // panics.
    for utterance in [
        "change İstanbul to Istanbul",
        "replace STRASSE with Straße",
        "delete ΣΊΣΥΦΟΣ",
        "change Ǆ to dz",
    ] {
        let intent = parse(utterance);
        let _ = apply("İstanbul STRASSE ΣΊΣΥΦΟΣ Ǆ", &intent);
    }
}

#[test]
fn recase_variants() {
    assert_eq!(apply::recase("hello world", Case::Upper), "HELLO WORLD");
    assert_eq!(apply::recase("HELLO WORLD", Case::Lower), "hello world");
    assert_eq!(apply::recase("hello world", Case::Title), "Hello World");
    assert_eq!(apply::recase("hELLO WORLD", Case::Sentence), "Hello world");
}

// ---------------------------------------------------------------------
// the corpus: every claim, asserted against an exact string
// ---------------------------------------------------------------------

/// Three sentences of dictated prose: mixed capitalisation, no line breaks.
/// This is what a recogniser actually produces, and the absence of line
/// breaks is why the line scopes have to refuse rather than guess.
const PROSE: &str = "It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon";

/// Text that genuinely has lines, for the commands that need them.
const LINES: &str = "buy milk\nwalk the dog\ncall mum";

/// What a command should do to a given target.
#[derive(Debug)]
enum Outcome {
    /// Handled deterministically, producing exactly this text.
    Text(&'static str),
    /// Parsed, but the caller resolves it (undo ring, or a no-match report).
    /// The important half of the claim is that it did NOT become Freeform
    /// and did NOT invent an edit.
    NoText,
    /// Correctly left for a language model.
    Model,
}

/// The whole coverage claim, in one table.
///
/// Expected strings are spelled out rather than computed, so a refactor that
/// changes behaviour fails here instead of quietly agreeing with itself.
#[allow(clippy::type_complexity)]
fn corpus() -> Vec<(&'static str, &'static str, Outcome)> {
    use Outcome::*;
    vec![
        // ---- the original literal verbs, which must be unchanged ----
        (PROSE, "change quick to slow", NoText),
        ("the quick fox", "change quick to slow", Text("the slow fox")),
        ("the quick fox", "replace quick with slow", Text("the slow fox")),
        ("the quick fox", "swap quick for slow", Text("the slow fox")),
        ("the quick fox", "make quick into slow", Text("the slow fox")),
        ("a very long day", "delete very", Text("a long day")),
        ("a very long day", "remove very", Text("a long day")),
        ("a very long day", "get rid of very", Text("a long day")),
        ("hello", "add world", Text("hello world")),
        ("hello", "append world", Text("hello world")),
        ("hello world", "make it all caps", Text("HELLO WORLD")),
        ("hello world", "make it title case", Text("Hello World")),
        ("HELLO WORLD", "lowercase everything", Text("hello world")),
        ("hELLO WORLD", "sentence case please", Text("Hello world")),
        // An ordinal WORD that is the operand, not a targeting phrase. The
        // ordinal-occurrence guard must not eat this: a workspace pipeline
        // test caught it doing so.
        ("first line\nsecond line", "change second to 2nd", Text("first line\n2nd line")),

        // ---- scoped delete ----
        (PROSE, "delete the last sentence", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset.")),
        (PROSE, "remove the first sentence", Text("The customers might possibly be quite upset. we should tell them soon")),
        (PROSE, "delete the second sentence", Text("It is really quite important that we should try to make sure the deploy happens today. we should tell them soon")),
        (PROSE, "delete the last word", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them")),
        (PROSE, "delete the first word", Text("is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon")),
        (PROSE, "get rid of the last sentence", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset.")),
        (LINES, "delete the first line", Text("walk the dog\ncall mum")),
        (LINES, "delete the last line", Text("buy milk\nwalk the dog")),
        // A line scope against unbroken prose has no answer, so it reports
        // rather than wiping the field.
        (PROSE, "delete the first line", NoText),
        // A sentence that is not there.
        (PROSE, "delete the fifth sentence", NoText),

        // ---- punctuation ----
        (PROSE, "add a period at the end", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon.")),
        (PROSE, "add a question mark", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon?")),
        (PROSE, "put a full stop at the end", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon.")),
        (PROSE, "add a comma after today", Text("It is really quite important that we should try to make sure the deploy happens today, The customers might possibly be quite upset. we should tell them soon")),
        (PROSE, "add a comma after the word today", Text("It is really quite important that we should try to make sure the deploy happens today, The customers might possibly be quite upset. we should tell them soon")),
        // Anchor absent: no edit, rather than an edit somewhere arbitrary.
        (PROSE, "add a comma after zebra", NoText),
        ("we ship today. they wait", "add a semicolon after today", Text("we ship today; they wait")),

        // ---- punctuation removal ----
        ("hello, world, again", "remove the last comma", Text("hello, world again")),
        ("hello, world, again", "delete the first comma", Text("hello world, again")),
        // No position named: which of the two commas? Refuse.
        ("hello, world, again", "remove the comma", Model),

        // ---- wrapping ----
        ("ship it today", "wrap this in quotes", Text("\"ship it today\"")),
        ("ship it today", "wrap that in backticks", Text("`ship it today`")),
        ("ship it today", "put this in parentheses", Text("(ship it today)")),
        ("ship it today", "surround this with square brackets", Text("[ship it today]")),
        ("ship it today", "wrap it in single quotes", Text("'ship it today'")),
        ("ship it today", "wrap this in bold", Text("**ship it today**")),
        // Already wrapped: doing it again is not what was meant.
        ("\"ship it today\"", "wrap this in quotes", NoText),

        // ---- identifier casing ----
        ("ship today. tell them", "make it snake case", Text("ship_today_tell_them")),
        ("ship today. tell them", "make it camel case", Text("shipTodayTellThem")),
        ("ship today. tell them", "make it kebab case", Text("ship-today-tell-them")),
        ("ship today. tell them", "make it pascal case", Text("ShipTodayTellThem")),
        ("ship today. tell them", "make it screaming snake case", Text("SHIP_TODAY_TELL_THEM")),
        ("ship today. tell them", "make this a slug", Text("ship-today-tell-them")),

        // ---- list and line operations ----
        (PROSE, "turn this into bullet points", Text("- It is really quite important that we should try to make sure the deploy happens today.\n- The customers might possibly be quite upset.\n- we should tell them soon")),
        (PROSE, "make this a bulleted list", Text("- It is really quite important that we should try to make sure the deploy happens today.\n- The customers might possibly be quite upset.\n- we should tell them soon")),
        (PROSE, "number these sentences", Text("1. It is really quite important that we should try to make sure the deploy happens today.\n2. The customers might possibly be quite upset.\n3. we should tell them soon")),
        (LINES, "number these lines", Text("1. buy milk\n2. walk the dog\n3. call mum")),
        (LINES, "join these lines", Text("buy milk walk the dog call mum")),
        (LINES, "put these on one line", Text("buy milk walk the dog call mum")),
        (PROSE, "split this into sentences", Text("It is really quite important that we should try to make sure the deploy happens today.\nThe customers might possibly be quite upset.\nwe should tell them soon")),
        (PROSE, "put each sentence on its own line", Text("It is really quite important that we should try to make sure the deploy happens today.\nThe customers might possibly be quite upset.\nwe should tell them soon")),
        // A spoken count is a promise a sentence splitter cannot keep.
        (PROSE, "split this into two lines", Model),
        // Nothing to join.
        (PROSE, "join these lines", NoText),
        // A one-item list is not what anyone asked for.
        ("just one sentence", "turn this into bullet points", NoText),

        // ---- scoped recasing ----
        ("it is important. they wait", "capitalize the first word", Text("It is important. they wait")),
        ("it is important. they wait", "uppercase the first letter", Text("It is important. they wait")),
        (PROSE, "make the last sentence title case", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. We Should Tell Them Soon")),
        (LINES, "make the first line title case", Text("Buy Milk\nwalk the dog\ncall mum")),
        (PROSE, "uppercase the last word", Text("It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them SOON")),
        // "capitalize the first sentence" could mean Title Case Every Word
        // or raise one letter. Ambiguous, so refused.
        (PROSE, "capitalize the first sentence", Model),

        // ---- scoped inner command ----
        ("its fine. its broken", "in the last sentence change its to it's", Text("its fine. it's broken")),
        ("keep quick. change quick", "in the last sentence replace quick with slow", Text("keep quick. change slow")),
        // Casing in the replacement survives: the inner command is re-read
        // from the ORIGINAL utterance, not from lowercased tokens.
        ("its fine. its broken", "in the last sentence change its to It's", Text("its fine. It's broken")),
        ("a b. a b", "within the first sentence change a to z", Text("z b. a b")),
        // A scope wrapped around an instruction we cannot execute is not
        // progress; it moves the guess one level down.
        (PROSE, "in the last sentence make it sound friendlier", Model),
        // A scope with nothing after it names no operation.
        (PROSE, "in the last sentence", Model),

        // ---- undo ----
        (PROSE, "undo that", NoText),
        (PROSE, "never mind", NoText),
        (PROSE, "scratch that", NoText),
        (PROSE, "go back to the original", NoText),

        // ---- whole-target delete ----
        (PROSE, "delete everything", Text("")),
        (PROSE, "get rid of all of this", Text("")),

        // ---- controls: genuinely open-ended, must escalate ----
        (PROSE, "tighten this up", Model),
        (PROSE, "make it more formal", Model),
        (PROSE, "summarize this", Model),
        (PROSE, "translate this to spanish", Model),
        (PROSE, "fix the grammar", Model),
        (PROSE, "make it sound friendlier", Model),
        (PROSE, "rewrite this as an apology", Model),
        (PROSE, "explain this more simply", Model),
        (PROSE, "make it past tense", Model),
        (PROSE, "expand on the second point", Model),

        // ---- ambiguity controls: nearly understood, deliberately refused --
        // Names a scope the wrap rule cannot honour; wrapping everything
        // would be a confident wrong edit.
        (PROSE, "wrap the last sentence in quotes", Model),
        // Same, for identifier casing, which destroys all spacing.
        (PROSE, "make the last word snake case", Model),
        // Two scopes: this grammar resolves neither rather than half of it.
        (PROSE, "make the first word of the last sentence title case", Model),
        // "add a period to the second sentence" names a placement the
        // punctuation rule cannot compute.
        (PROSE, "add a period to the second sentence", Model),
        // Names two marks; means a replacement this grammar lacks.
        (PROSE, "change the period to a comma", Model),
        // A scoped delete with trailing words we did not understand.
        (PROSE, "delete the last sentence and the one before it", Model),
        // Names a scope but no operation.
        (PROSE, "the last sentence", Model),

        // ---- phrasings this grammar does NOT handle, and does not pretend
        // to. Recorded so the gap is visible rather than discovered by a
        // user. Each one escalates, which is the safe outcome. ----
        // Ordinal targeting of an occurrence rather than a unit.
        (PROSE, "change the second the to a", Model),
        // Positional insertion other than "after".
        (PROSE, "add a comma before customers", Model),
        // Moving text.
        (PROSE, "move the last sentence to the top", Model),
        // Swapping two units.
        (PROSE, "swap the first and last sentences", Model),
        // Counted units.
        (PROSE, "delete the last two words", Model),
        // Un-wrapping. Read literally as a search for the words "the
        // quotes", which finds nothing and reports no-match: not handled,
        // but safe.
        ("\"quoted\"", "remove the quotes", NoText),
        // Whole-word replacement semantics: this grammar replaces
        // substrings, so "change the to a" would also hit "them". The
        // literal verb claims it, and that is pre-existing behaviour rather
        // than a regression, but it is a real gap.
        (PROSE, "capitalize every sentence", Model),
    ]
}

/// Run the corpus and return (handled, escalated, failures).
fn run_corpus() -> (usize, usize, Vec<String>) {
    let mut handled = 0;
    let mut escalated = 0;
    let mut failures = Vec::new();
    for (target, utterance, expect) in corpus() {
        let intent = parse(utterance);
        let is_freeform = matches!(intent, EditIntent::Freeform { .. });
        let got = apply(target, &intent);
        match (&expect, is_freeform, &got) {
            (Outcome::Text(want), false, Some(got)) if got == want => handled += 1,
            (Outcome::NoText, false, None) => handled += 1,
            (Outcome::Model, true, _) => escalated += 1,
            (Outcome::Text(want), _, _) => failures.push(format!(
                "{utterance:?}\n      want: {want:?}\n       got: {got:?} (intent {intent:?})"
            )),
            (Outcome::NoText, _, _) => failures.push(format!(
                "{utterance:?} should parse but produce no text; got {got:?} (intent {intent:?})"
            )),
            (Outcome::Model, _, _) => failures.push(format!(
                "{utterance:?} should escalate but parsed as {intent:?}"
            )),
        }
    }
    (handled, escalated, failures)
}

/// The headline coverage claim, asserted rather than estimated.
#[test]
fn corpus_produces_exactly_the_expected_strings() {
    let (handled, escalated, failures) = run_corpus();
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n  ")
    );
    assert_eq!(
        handled + escalated,
        corpus().len(),
        "every case must be accounted for"
    );
    // Pinned so a change to the grammar has to be a deliberate edit here.
    // The numbers are the coverage claim quoted in
    // `docs/investigations/edit-intent-scope.md`: 73 of 101 realistic
    // phrasings are handled deterministically, and the other 28 escalate,
    // every one of them either genuinely open-ended or deliberately refused
    // as ambiguous.
    assert_eq!(handled, 73, "deterministic coverage changed");
    assert_eq!(escalated, 28, "escalation set changed");
}

/// The regression that made this work necessary: a command scoped to one
/// letter must not rewrite the field.
#[test]
fn a_scoped_command_never_edits_outside_its_scope() {
    let before = "it is important. they wait";
    let after = apply(before, &parse("uppercase the first letter")).unwrap();
    assert_eq!(after, "It is important. they wait");
    // Everything after the first character is byte-identical.
    assert_eq!(&after[1..], &before[1..]);
}

/// The other headline regression: appending the words of the command.
#[test]
fn add_a_period_adds_a_period_not_the_words() {
    let got = apply(PROSE, &parse("add a period at the end")).unwrap();
    assert!(got.ends_with("soon."), "{got}");
    assert!(!got.contains("a period at the end"), "{got}");
}

/// Punctuation must never stack.
#[test]
fn punctuation_does_not_stack() {
    let got = apply(
        "we ship today. they wait",
        &parse("add a comma after today"),
    )
    .unwrap();
    assert_eq!(got, "we ship today, they wait");
    let got = apply("all done.", &parse("add a question mark")).unwrap();
    assert_eq!(got, "all done?");
}

/// A scoped replace must not touch matching text outside the scope. This is
/// what `docs/ux/03-edit-by-voice.md` promises by "in the last sentence,
/// change its to it's", and a scope that leaks is worse than no scope at all
/// because the user believes they narrowed it.
#[test]
fn scope_narrowing_is_real_not_cosmetic() {
    let got = apply(
        "its fine. its ok. its broken",
        &parse("in the last sentence change its to it's"),
    )
    .unwrap();
    assert_eq!(got, "its fine. its ok. it's broken");
}

/// The prototype's known limitation, now fixed: an abbreviation must not end
/// a sentence, or a scoped delete takes the wrong words.
#[test]
fn sentence_scope_survives_abbreviations_and_decimals() {
    let text = "Ship at 3.5 percent. Tell Dr. Smith we are done";
    let got = apply(text, &parse("delete the last sentence")).unwrap();
    assert_eq!(got, "Ship at 3.5 percent.");
    let got = apply(text, &parse("delete the first sentence")).unwrap();
    assert_eq!(got, "Tell Dr. Smith we are done");
}

/// A scoped delete must repair only the seam it made. The prototype
/// collapsed all whitespace in the whole target, silently reflowing text
/// far from the edit.
#[test]
fn scoped_delete_preserves_distant_whitespace() {
    let text = "keep  this\nand this too\nremove me";
    let got = apply(text, &parse("delete the last line")).unwrap();
    assert_eq!(got, "keep  this\nand this too");
}

/// Undo parses as a distinct intent rather than as a literal delete. The
/// old grammar read "scratch that" as `Delete { text: "that" }`, which
/// removes every occurrence of the word.
#[test]
fn undo_phrases_are_not_literal_deletes() {
    for (utterance, depth) in [
        ("undo that", UndoDepth::One),
        ("never mind", UndoDepth::One),
        ("scratch that", UndoDepth::One),
        ("go back to the original", UndoDepth::All),
        ("start over", UndoDepth::All),
    ] {
        assert_eq!(parse(utterance), EditIntent::Undo(depth), "{utterance}");
        assert!(
            apply("that thing. that other thing", &parse(utterance)).is_none(),
            "{utterance} must not transform text"
        );
    }
}

/// "delete this" is deliberately NOT a whole-target delete. It is equally a
/// literal request to remove the word "this", and the literal reading fails
/// safe (no match, nothing written) where the destructive reading wipes the
/// user's selection.
#[test]
fn bare_delete_this_keeps_its_safe_literal_reading() {
    assert_eq!(
        parse("delete this"),
        EditIntent::Delete {
            text: "this".into()
        }
    );
    assert!(apply("no such word here", &parse("delete this")).is_none());
}

/// Identifier casing must emit only identifier characters, which is exactly
/// where a language model failed in the investigation's head-to-head.
#[test]
fn identifier_casing_drops_punctuation() {
    for (utterance, want) in [
        ("make it snake case", "ship_today_tell_them"),
        ("make it kebab case", "ship-today-tell-them"),
        ("make it camel case", "shipTodayTellThem"),
    ] {
        assert_eq!(
            apply("Ship today. Tell them!", &parse(utterance)).unwrap(),
            want
        );
    }
}

/// A scoped delete can legitimately empty the target, and that is not a bug
/// to be papered over here.
///
/// "delete the last sentence" against a field holding exactly one sentence
/// means delete it. Refusing would be wrong. But the blast radius is 100% of
/// the target, which is precisely the case `docs/ux/03-edit-by-voice.md`
/// routes to a preview rather than an instant apply, so this is pinned to
/// make the hand-off explicit: the parser's job is to be right, and the
/// delivery layer's job is to decide whether "right and total" needs
/// confirming.
#[test]
fn a_scoped_delete_may_legitimately_empty_the_target() {
    assert_eq!(
        apply(
            "the quick brown fox jumps over the lazy dog",
            &parse("delete the last sentence")
        ),
        Some(String::new())
    );
    assert_eq!(
        apply(
            "only one sentence here.",
            &parse("delete the first sentence")
        ),
        Some(String::new())
    );
}

/// Degenerate targets must not panic or fabricate text.
#[test]
fn degenerate_targets_are_safe() {
    for target in ["", "   ", "\n", "."] {
        for (utterance, _, _) in corpus() {
            let _ = apply(target, &parse(utterance));
        }
    }
}

/// Every command shape, run against non-ASCII targets whose case folding
/// changes byte length. This is the class the workspace fuzz suite found in
/// this crate once already, and scope resolution does far more byte-offset
/// arithmetic than the original grammar did.
#[test]
fn non_ascii_targets_never_panic() {
    for target in [
        "İstanbul ΣΊΣΥΦΟΣ. Straße Ǆ",
        "日本語の文。もう一つ",
        "emoji 🎉 sentence. another 🚀 one",
        "e\u{301}\u{200b} test. more",
    ] {
        for (_, utterance, _) in corpus() {
            let _ = apply(target, &parse(utterance));
        }
    }
}
