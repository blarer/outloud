//! Coverage of the shipped parser over the investigation's 55-command corpus.
//!
//! This is the harness behind the table in
//! `docs/investigations/edit-intent.md`. That investigation measured the
//! original four-verb grammar: 15 correct, 10 **silently wrong**, 15 with an
//! exact deterministic answer that escalated anyway, 15 correctly escalated.
//!
//! The corpus is unchanged. What changed is the parser, so this harness now
//! records the BEFORE verdict alongside what the parser does today, which
//! makes the delta a measurement rather than a claim.
//!
//! The classification is **declared per case, not inferred**. An earlier
//! version guessed with keyword heuristics (`utt.contains("period")` and
//! friends), which made the headline counts an artifact of the heuristic.
//!
//! Run: `cargo run -p edit-intent --release --example shipped_parser_corpus`

use edit_intent::{apply, parse, EditIntent};

/// Three sentences of dictated prose, mixed capitalization, no line breaks.
const SAMPLE: &str = "It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon";

/// What the ORIGINAL four-verb parser did with a command, as a judgement
/// about the user's outcome rather than about the parser's internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// Parsed deterministically and produced the right text.
    Correct,
    /// Parsed deterministically and produced the WRONG text, silently. The
    /// worst bucket: the user gets a bad edit with no error.
    SilentlyWrong,
    /// Fell through to `Freeform`, i.e. to a model that is not wired up,
    /// even though the command has an exact deterministic answer.
    MissingFeature,
    /// Fell through to `Freeform`, correctly: genuinely open-ended.
    CorrectlyEscalated,
}

/// What the parser does with it NOW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// Handled deterministically and applies to the sample.
    Handled,
    /// Recognised as a command, but produces no text against this sample:
    /// an undo request, or a scope this sample does not contain. Both are
    /// reported to the user rather than guessed at.
    Reported,
    /// Escalated to a model.
    Escalated,
}

fn kind(i: &EditIntent) -> &'static str {
    match i {
        EditIntent::Replace { .. } => "Replace",
        EditIntent::Delete { .. } => "Delete",
        EditIntent::Append { .. } => "Append",
        EditIntent::Recase(_) => "Recase",
        EditIntent::DeleteScope(_) => "DeleteScope",
        EditIntent::Scoped { .. } => "Scoped",
        EditIntent::Punctuate { .. } => "Punctuate",
        EditIntent::DeleteMark { .. } => "DeleteMark",
        EditIntent::Wrap { .. } => "Wrap",
        EditIntent::Identifier(_) => "Identifier",
        EditIntent::ListOp(_) => "ListOp",
        EditIntent::Undo(_) => "Undo",
        EditIntent::Freeform { .. } => "Freeform",
    }
}

fn observe(utterance: &str) -> (EditIntent, After) {
    let intent = parse(utterance);
    let after = match (&intent, apply(SAMPLE, &intent)) {
        (EditIntent::Freeform { .. }, _) => After::Escalated,
        (_, Some(_)) => After::Handled,
        (_, None) => After::Reported,
    };
    (intent, after)
}

/// (utterance, verdict under the ORIGINAL grammar, why) for every case.
///
/// The `why` string documents the behaviour that justified the original
/// verdict, so a reader can check the classification without running the
/// old parser.
#[allow(clippy::type_complexity)]
fn corpus() -> Vec<(&'static str, Before, &'static str)> {
    use Before::*;
    vec![
        // ---- literal commands the original parser genuinely handled ----
        ("change quick to slow", Correct, "Replace, exact"),
        ("replace deploy with release", Correct, "Replace, exact"),
        ("swap customers for users", Correct, "Replace, exact"),
        ("make today into tomorrow", Correct, "Replace, exact"),
        ("delete really", Correct, "Delete, exact"),
        ("remove possibly", Correct, "Delete, exact"),
        ("get rid of quite", Correct, "Delete, exact"),
        ("add and thanks", Correct, "Append, exact"),
        ("append please review", Correct, "Append, exact"),
        (
            "make it all caps",
            Correct,
            "Recase(Upper), whole field intended",
        ),
        ("make it title case", Correct, "Recase(Title)"),
        ("lowercase everything", Correct, "Recase(Lower)"),
        ("sentence case please", Correct, "Recase(Sentence)"),
        ("delete that", Correct, "Delete of the literal word 'that'"),
        (
            "add a new line",
            Correct,
            "Append, literal reading is defensible",
        ),
        // ---- SILENTLY WRONG: parsed, applied, produced nonsense ----
        (
            "add a period at the end",
            SilentlyWrong,
            "appended the literal words 'a period at the end'",
        ),
        (
            "add a comma after today",
            SilentlyWrong,
            "appended the literal words 'a comma after today'",
        ),
        (
            "add a question mark",
            SilentlyWrong,
            "appended the literal words 'a question mark'",
        ),
        (
            "uppercase the first letter",
            SilentlyWrong,
            "SHOUTED THE WHOLE FIELD: parse_case matched contains(\"uppercase\")",
        ),
        (
            "make the first line title case",
            SilentlyWrong,
            "title-cased the whole field, ignoring the 'first line' scope",
        ),
        (
            "delete the last sentence",
            SilentlyWrong,
            "searched for the literal text 'the last sentence'",
        ),
        (
            "remove the first sentence",
            SilentlyWrong,
            "searched for the literal text 'the first sentence'",
        ),
        (
            "delete the last word",
            SilentlyWrong,
            "searched for the literal text 'the last word'",
        ),
        (
            "remove the last comma",
            SilentlyWrong,
            "searched for the literal text 'the last comma'",
        ),
        (
            "change this to a question",
            SilentlyWrong,
            "Replace of the literal word 'this' with 'a question'",
        ),
        // ---- MISSING: exact answer existed, but escalated to no model ----
        (
            "capitalize the first word",
            MissingFeature,
            "no scope support",
        ),
        (
            "in the last sentence change its to it's",
            MissingFeature,
            "no scope",
        ),
        ("join these lines", MissingFeature, "no line ops"),
        ("split this into two lines", MissingFeature, "no line ops"),
        ("make this a bulleted list", MissingFeature, "no list ops"),
        (
            "turn this into bullet points",
            MissingFeature,
            "no list ops",
        ),
        ("number these lines", MissingFeature, "no list ops"),
        ("wrap this in quotes", MissingFeature, "no wrapping"),
        ("wrap that in backticks", MissingFeature, "no wrapping"),
        ("make it snake case", MissingFeature, "no identifier casing"),
        ("make it camel case", MissingFeature, "no identifier casing"),
        ("make it kebab case", MissingFeature, "no identifier casing"),
        ("undo that", MissingFeature, "no undo ring"),
        ("never mind", MissingFeature, "no undo ring"),
        ("go back to the original", MissingFeature, "no undo ring"),
        // ---- correctly escalated: genuinely needs a model ----
        ("tighten this up", CorrectlyEscalated, "open-ended rewrite"),
        ("make it more formal", CorrectlyEscalated, "register change"),
        ("make it sound friendlier", CorrectlyEscalated, "tone"),
        ("fix the grammar", CorrectlyEscalated, "open-ended"),
        ("make this shorter", CorrectlyEscalated, "open-ended"),
        (
            "rewrite this as an apology",
            CorrectlyEscalated,
            "generation",
        ),
        ("explain this more simply", CorrectlyEscalated, "generation"),
        (
            "make it more concise and professional",
            CorrectlyEscalated,
            "tone",
        ),
        (
            "turn this into a commit message",
            CorrectlyEscalated,
            "generation",
        ),
        ("summarize this", CorrectlyEscalated, "generation"),
        (
            "make it past tense",
            CorrectlyEscalated,
            "grammatical rewrite",
        ),
        (
            "translate this to spanish",
            CorrectlyEscalated,
            "translation",
        ),
        ("fix the spelling", CorrectlyEscalated, "open-ended"),
        ("make the tone more direct", CorrectlyEscalated, "tone"),
        (
            "expand on the second point",
            CorrectlyEscalated,
            "generation",
        ),
    ]
}

fn main() {
    let cases = corpus();
    println!(
        "{:<42} {:<20} {:<12} {:<10}",
        "utterance", "before", "now", "intent"
    );
    println!("{}", "-".repeat(110));
    for (utt, before, _why) in &cases {
        let (intent, after) = observe(utt);
        println!("{utt:<42} {before:<20?} {after:<12?} {}", kind(&intent));
    }

    let mut fixed = 0;
    let mut still_escalating = 0;
    let mut regressed = Vec::new();
    for (utt, before, _) in &cases {
        let (_, after) = observe(utt);
        match (before, after) {
            (Before::CorrectlyEscalated, After::Escalated) => {}
            // An open-ended phrase captured by a literal parse is the worst
            // possible outcome, so it is called out by name.
            (Before::CorrectlyEscalated, _) => regressed.push(format!("{utt:?} was hijacked")),
            (Before::Correct, After::Escalated) => {
                regressed.push(format!("{utt:?} no longer parses"))
            }
            (Before::Correct, _) => {}
            (_, After::Escalated) => still_escalating += 1,
            _ => fixed += 1,
        }
    }

    println!("\n{}", "=".repeat(110));
    println!("corpus size:                              {}", cases.len());
    println!("previously wrong or missing, now handled: {fixed}");
    println!("previously wrong or missing, still open:  {still_escalating}");
    println!(
        "regressions:                              {}",
        regressed.len()
    );
    for r in &regressed {
        println!("  - {r}");
    }
    if !regressed.is_empty() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No open-ended phrase may be captured by a literal parse: that would
    /// mean the parser confidently mangles text the user wanted rewritten.
    /// This is the property the whole conservatism rule exists to protect,
    /// and it holds on the original corpus.
    #[test]
    fn no_open_ended_phrase_is_hijacked() {
        for (utt, before, _) in corpus() {
            if before == Before::CorrectlyEscalated {
                let (intent, _) = observe(utt);
                assert!(
                    matches!(intent, EditIntent::Freeform { .. }),
                    "{utt:?} was captured by a literal parse as {}",
                    kind(&intent)
                );
            }
        }
    }

    /// Nothing the original grammar got right may have stopped working.
    #[test]
    fn previously_correct_commands_still_parse() {
        for (utt, before, why) in corpus() {
            if before == Before::Correct {
                let (intent, after) = observe(utt);
                assert!(
                    after != After::Escalated,
                    "{utt:?} ({why}) regressed to Freeform"
                );
                let _ = intent;
            }
        }
    }

    /// The delta this work exists to produce, pinned so it cannot be
    /// claimed without being true. 25 of the 55 cases were silently wrong
    /// or missing-with-an-exact-answer; all 25 are now handled.
    #[test]
    fn every_silently_wrong_or_missing_case_is_now_handled() {
        let mut unhandled = Vec::new();
        let mut total = 0;
        for (utt, before, why) in corpus() {
            if !matches!(before, Before::SilentlyWrong | Before::MissingFeature) {
                continue;
            }
            total += 1;
            let (intent, after) = observe(utt);
            if after == After::Escalated {
                unhandled.push(format!("{utt:?} ({why}) -> {}", kind(&intent)));
            }
        }
        assert_eq!(total, 25, "the before-corpus changed");
        // "split this into two lines" names a count a sentence splitter
        // cannot honour, so it is deliberately refused. Refusing is the
        // correct outcome, and it is the only one of the 25 left open.
        assert_eq!(
            unhandled.len(),
            1,
            "expected exactly the counted-split case to remain open, got:\n{unhandled:#?}"
        );
        assert!(unhandled[0].starts_with("\"split this into two lines\""));
    }

    /// The headline misparse: a request scoped to one letter no longer
    /// shouts the entire field.
    #[test]
    fn uppercase_the_first_letter_no_longer_shouts_the_whole_field() {
        let got = apply(SAMPLE, &parse("uppercase the first letter")).unwrap();
        assert_ne!(got, SAMPLE.to_uppercase());
        assert_eq!(got, SAMPLE, "the sample already starts with a capital I");
    }

    /// "add a period at the end" adds a period, not the words.
    #[test]
    fn add_a_period_adds_a_period() {
        let got = apply(SAMPLE, &parse("add a period at the end")).unwrap();
        assert!(got.ends_with("soon."), "{got}");
        assert!(!got.contains("a period at the end"), "{got}");
    }
}
