//! Where does the SHIPPED parser succeed, fail silently, or give up?
//!
//! This is the harness behind the 54-command table in
//! docs/investigations/edit-intent.md. It runs a realistic corpus of spoken
//! edit commands through `edit_intent::parse` + `apply` and classifies each
//! outcome into one of four buckets.
//!
//! The classification is **declared per case, not inferred**. An earlier
//! version of this harness guessed at "is this result wrong?" with keyword
//! heuristics (`utt.contains("period")` and friends), which meant the
//! headline counts were an artifact of the heuristic rather than a
//! measurement. Every case now carries an explicit expected classification,
//! and the run asserts the totals, so the numbers quoted in the
//! investigation cannot drift from what the parser actually does.
//!
//! Run: `cargo run -p edit-intent --release --example shipped_parser_corpus`

use edit_intent::{apply, parse, EditIntent};

/// Three sentences of dictated prose, mixed capitalization, no line breaks.
const SAMPLE: &str = "It is really quite important that we should try to make sure the deploy happens today. The customers might possibly be quite upset. we should tell them soon";

/// What the SHIPPED parser does with a command, as a judgement about the
/// user's outcome rather than about the parser's internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
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

fn kind(i: &EditIntent) -> &'static str {
    match i {
        EditIntent::Replace { .. } => "Replace",
        EditIntent::Delete { .. } => "Delete",
        EditIntent::Append { .. } => "Append",
        EditIntent::Recase(_) => "Recase",
        EditIntent::Freeform { .. } => "Freeform",
    }
}

/// (utterance, expected verdict, why) for every case in the corpus.
///
/// The `why` string documents the *observed* behaviour that justifies the
/// verdict, so a reader can check the classification without running it.
#[allow(clippy::type_complexity)]
fn corpus() -> Vec<(&'static str, Verdict, &'static str)> {
    use Verdict::*;
    vec![
        // ---- literal commands the parser genuinely handles ----
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
        // ---- SILENTLY WRONG: parses, applies, produces nonsense ----
        (
            "add a period at the end",
            SilentlyWrong,
            "appends the literal words 'a period at the end'",
        ),
        (
            "add a comma after today",
            SilentlyWrong,
            "appends the literal words 'a comma after today'",
        ),
        (
            "add a question mark",
            SilentlyWrong,
            "appends the literal words 'a question mark'",
        ),
        (
            "uppercase the first letter",
            SilentlyWrong,
            "SHOUTS THE WHOLE FIELD: parse_case matches contains(\"uppercase\")",
        ),
        (
            "make the first line title case",
            SilentlyWrong,
            "title-cases the whole field, ignoring the 'first line' scope",
        ),
        (
            "delete the last sentence",
            SilentlyWrong,
            "searches for the literal text 'the last sentence'",
        ),
        (
            "remove the first sentence",
            SilentlyWrong,
            "searches for the literal text 'the first sentence'",
        ),
        (
            "delete the last word",
            SilentlyWrong,
            "searches for the literal text 'the last word'",
        ),
        (
            "remove the last comma",
            SilentlyWrong,
            "searches for the literal text 'the last comma'",
        ),
        (
            "change this to a question",
            SilentlyWrong,
            "Replace of the literal word 'this' with 'a question'",
        ),
        // ---- MISSING: exact answer exists, but escalates to no model ----
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

/// Check that the parser's actual behaviour is CONSISTENT with the declared
/// verdict. This cannot judge "is the text what the user meant" (that is the
/// human judgement encoded in the table), but it can catch the table going
/// stale: a case declared Freeform-escalating that now parses literally, or
/// vice versa, is a contradiction worth failing on.
fn consistent(v: Verdict, parsed_freeform: bool) -> bool {
    match v {
        Verdict::MissingFeature | Verdict::CorrectlyEscalated => parsed_freeform,
        Verdict::Correct | Verdict::SilentlyWrong => !parsed_freeform,
    }
}

fn tally(cases: &[(&str, Verdict, &str)]) -> (usize, usize, usize, usize, Vec<String>) {
    let (mut ok, mut wrong, mut missing, mut escalated) = (0, 0, 0, 0);
    let mut contradictions = Vec::new();
    for (utt, verdict, _why) in cases {
        let intent = parse(utt);
        let is_freeform = matches!(intent, EditIntent::Freeform { .. });
        if !consistent(*verdict, is_freeform) {
            contradictions.push(format!(
                "{utt:?} is declared {verdict:?} but parsed as {}",
                kind(&intent)
            ));
        }
        match verdict {
            Verdict::Correct => ok += 1,
            Verdict::SilentlyWrong => wrong += 1,
            Verdict::MissingFeature => missing += 1,
            Verdict::CorrectlyEscalated => escalated += 1,
        }
    }
    (ok, wrong, missing, escalated, contradictions)
}

fn main() {
    let cases = corpus();
    println!("{:<42} {:<9} {:<19} why", "utterance", "parsed", "verdict");
    println!("{}", "-".repeat(120));
    for (utt, verdict, why) in &cases {
        let intent = parse(utt);
        println!("{utt:<42} {:<9} {:<19?} {why}", kind(&intent), verdict);
    }

    let (ok, wrong, missing, escalated, contradictions) = tally(&cases);
    println!("\n{}", "=".repeat(120));
    println!("corpus size:                        {}", cases.len());
    println!("deterministic and correct:          {ok}");
    println!("deterministic but SILENTLY WRONG:   {wrong}");
    println!("should be deterministic, MISSING:   {missing}");
    println!("correctly escalated (needs model):  {escalated}");

    // A literal parse of an open-ended phrase would be the worst failure of
    // all, so it is called out even though the count is zero.
    let hijacked = cases
        .iter()
        .filter(|(u, v, _)| {
            *v == Verdict::CorrectlyEscalated && !matches!(parse(u), EditIntent::Freeform { .. })
        })
        .count();
    println!("open-ended HIJACKED by the parser:  {hijacked}");

    // Show that the deterministic buckets really do produce output, and the
    // escalating ones really do not.
    let applies = cases
        .iter()
        .filter(|(u, _, _)| apply(SAMPLE, &parse(u)).is_some())
        .count();
    println!(
        "\ncases producing any output on the sample: {applies}/{}",
        cases.len()
    );

    if contradictions.is_empty() {
        println!("\nno contradictions: every declared verdict matches how the parser parses.");
    } else {
        println!("\n{} CONTRADICTIONS:", contradictions.len());
        for c in &contradictions {
            println!("  - {c}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 55-command table quoted in docs/investigations/edit-intent.md.
    /// If the shipped parser changes, this fails and the doc must be updated.
    #[test]
    fn corpus_totals_match_the_investigation() {
        let cases = corpus();
        let (ok, wrong, missing, escalated, contradictions) = tally(&cases);
        assert!(contradictions.is_empty(), "{contradictions:#?}");
        assert_eq!(cases.len(), 55, "corpus size");
        assert_eq!(ok, 15, "deterministic and correct");
        assert_eq!(wrong, 10, "deterministic but silently wrong");
        assert_eq!(missing, 15, "should be deterministic but escalates");
        assert_eq!(escalated, 15, "correctly escalated");
    }

    /// No open-ended phrase may be captured by a literal parse: that would
    /// mean the parser confidently mangles text the user wanted rewritten.
    #[test]
    fn no_open_ended_phrase_is_hijacked() {
        for (utt, verdict, _) in corpus() {
            if verdict == Verdict::CorrectlyEscalated {
                assert!(
                    matches!(parse(utt), EditIntent::Freeform { .. }),
                    "{utt:?} was captured by a literal parse"
                );
            }
        }
    }

    /// The headline claim of finding (b) in the investigation: the worst
    /// misparse shouts the entire field for a request scoped to one letter.
    #[test]
    fn uppercase_the_first_letter_shouts_the_whole_field() {
        let got = apply(SAMPLE, &parse("uppercase the first letter")).unwrap();
        assert_eq!(got, SAMPLE.to_uppercase());
    }

    /// "add a period at the end" appends the words rather than a period.
    #[test]
    fn add_a_period_appends_the_words() {
        let got = apply(SAMPLE, &parse("add a period at the end")).unwrap();
        assert!(got.ends_with("a period at the end"), "{got}");
        assert!(!got.ends_with("soon."), "{got}");
    }
}
