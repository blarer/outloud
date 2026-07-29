//! Telling "the user dictated while a stale selection happened to exist"
//! apart from "the user issued an instruction ABOUT the selection".
//!
//! ## Why this module exists
//!
//! A selection at key-down means an edit is POSSIBLE, not that one was
//! INTENDED, and the two readings of the same utterance have opposite
//! correct behaviours:
//!
//! - *"The customers might possibly be quite upset."* spoken while a
//!   terminal still holds yesterday's drag highlighted is **dictation**.
//!   Refusing it writes nothing and reads as "the app stopped
//!   transcribing", which shipped once and was a real user-facing failure.
//! - *"tighten this up"* spoken with that same sentence selected is an
//!   **instruction about the selection**. Inserting it replaces the
//!   sentence with the words describing what should have happened to it.
//!   That shipped too, and it silently destroys a paragraph.
//!
//! The delivery path previously could not distinguish them, so it picked
//! one reading globally and paid the other's cost. This module makes the
//! distinction explicit, closed-vocabulary, and unit-testable.
//!
//! ## The guiding asymmetry
//!
//! **A wrong refusal costs the user one retry. A wrong overwrite costs
//! them their paragraph, silently.** So the classifier is deliberately
//! biased toward calling something a rewrite request: that branch writes
//! NOTHING and surfaces a visible error naming what was heard. Every
//! false refusal has a one-utterance escape hatch (`docs/ux/03`'s
//! documented `type:` prefix), and no false refusal can lose text.
//!
//! ## The rule
//!
//! Signals available at this boundary, and which are actually used:
//!
//! | Signal | Used | Why / why not |
//! |---|---|---|
//! | Utterance opens with a rewrite verb | yes | The strongest available signal, and a closed set |
//! | Utterance refers to the selection (`this`/`it`/`that`, or a property of the text like `grammar`) | yes | Distinguishes "fix it" / "fix the grammar" (meta) from "fix the login bug" (prose) |
//! | Utterance is short | yes | Instructions are terse; dictated prose is not bounded |
//! | Utterance is short *relative to the selection* | yes | Separately, as a blast-radius guard: a few words replacing a whole document is a deletion, not an edit |
//! | Selection changed recently | no | AX exposes no selection timestamp, and polling to synthesise one would race the user's own mouse. Considered and rejected as unavailable rather than unhelpful |
//!
//! Two verb tiers, because the verbs differ in how often they open
//! ordinary prose:
//!
//! - **Meta-only verbs** (`tighten`, `summarize`, `rephrase`, ...) are
//!   essentially never the first word of dictated prose, so they classify
//!   as a rewrite request on their own.
//! - **Ambiguous verbs** (`fix`, `make`, `clean`, ...) open prose all the
//!   time ("fix the login bug and add tests"), so they additionally
//!   require a reference to the selection and a short utterance.
//!
//! ## Measured, not asserted
//!
//! `cargo run -p outloud --example freeform_stress` runs a corpus built
//! to be adversarial in both directions (prose that STARTS with an
//! ambiguous verb; terse instructions, several without a pronoun):
//!
//! | | rate |
//! |---|---|
//! | destructive misses (an instruction written over the selection) | **0/15** |
//! | false refusals (prose refused, costs one retry) | **2/19** |
//!
//! The false-refusal figure was 6/19 in the first measured version. Two
//! changes brought it down without costing a single destructive miss:
//! demoting the meta-verbs that readily open prose (`polish`,
//! `translate`, `simplify`, ...) to the corroboration-required tier, and
//! requiring the selection reference to sit in the verb's OBJECT
//! position rather than anywhere in the sentence. Both were found by
//! running the corpus, not by reasoning about it.
//!
//! The two remaining false refusals ("make it happen by Friday please",
//! "fix that leak in the kitchen sink this weekend") are genuinely
//! ambiguous without context, and both cost exactly one retry via the
//! `type:` prefix. That is the trade this module exists to make.
//!
//! ## Blast radius, separately from wording
//!
//! Wording is not the only way this can be catastrophic. Writing a
//! dictated phrase REPLACES the selection, so five words landing on a
//! five-hundred-word selection deletes the document even though the
//! wording was read correctly as dictation. `docs/ux/03` already treats
//! scale as ambiguity for deterministic edits ("when the parse succeeds
//! but the result would be destructive and enormous"); the same rule
//! applies here, and applies harder, because this reading was uncertain
//! to begin with. See `MAX_SELECTION_TO_UTTERANCE_RATIO`.
//!
//! Nothing here writes, reads the accessibility tree, or touches a
//! window: it is a pure function of two strings, which is what lets the
//! destructive case be tested without a focused UI element.

/// What should happen to a transcript that reached the edit path but did
/// not parse as a deterministic command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreeformDisposition {
    /// Ordinary dictation that happened to occur while something was
    /// selected. Insert it. `text` is the transcript to write, which is
    /// the utterance minus any explicit `type:` dictation prefix.
    Dictate { text: String },
    /// Recognisably an instruction about the selection that no
    /// deterministic command can serve. Write nothing and say so: the
    /// user's selected text is the one thing that must survive.
    RewriteRequest { instruction: String },
}

/// Verbs that essentially only ever introduce an instruction about
/// existing text. Someone dictating prose does not open with them, so one
/// of these in the leading position is enough on its own.
///
/// Deliberately a closed list rather than a heuristic: a list can be
/// reviewed, and a wrong entry costs a retry rather than a paragraph.
const META_VERBS: &[&str] = &[
    "tighten",
    "shorten",
    "lengthen",
    "summarize",
    "summarise",
    "paraphrase",
    "rephrase",
    "reword",
    "rewrite",
    "proofread",
    "formalize",
    "formalise",
    "casualize",
    "reflow",
    "dedupe",
];

/// Verbs that introduce instructions *and* open ordinary sentences. These
/// need corroboration: a reference to the selection and a short utterance.
///
/// "fix the login bug and add tests" is the case that keeps `fix` here
/// rather than above; it is dictation and must keep being written.
/// Kept deliberately short. Every verb here buys protection for one
/// instruction phrasing and risks a false refusal on prose that opens the
/// same way, so a verb earns its place only if it is common in
/// instructions AND rare as the first word of dictated content.
const AMBIGUOUS_VERBS: &[&str] = &[
    "fix",
    "correct",
    "make",
    "turn",
    "clean",
    "improve",
    "clarify",
    "edit",
    "revise",
    "adjust",
    "soften",
    "strengthen",
    "trim",
    "phrase",
    // Demoted from META_VERBS after measurement. Each of these opens
    // ordinary prose readily enough that treating it as decisive caused
    // a false refusal in `examples/freeform_stress`: "polish the silver
    // before the dinner", "translate for the client if they ask",
    // "simplify the tax code he said". They still catch the instruction
    // phrasing ("polish this", "translate this to spanish") through the
    // corroboration rule.
    "polish",
    "translate",
    "condense",
    "compress",
    "abbreviate",
    "simplify",
    "elaborate",
    "reformat",
    "restructure",
    "rework",
    "punctuate",
    "tidy",
];

/// Words that point at the selection rather than naming new content.
/// "fix it" is about something already on screen; "fix the login bug" is
/// not.
const SELECTION_REFERENCES: &[&str] = &["this", "it", "that", "these", "those", "them", "above"];

/// Nouns that name a PROPERTY OF THE TEXT rather than new content, which
/// is the other way an utterance can be about the selection without a
/// pronoun. "fix the grammar" has no "this" in it and is still plainly an
/// instruction; "fix the login bug" names a thing in the world.
///
/// These count as a reference to the selection for the corroboration rule
/// below, and they are the reason that rule is not pronoun-only.
const TEXT_PROPERTY_NOUNS: &[&str] = &[
    "grammar",
    "spelling",
    "punctuation",
    "wording",
    "phrasing",
    "tone",
    "typo",
    "typos",
    "capitalization",
    "capitalisation",
    "formatting",
    "paragraph",
    "sentence",
    "wordy",
    "prose",
];

/// Politeness and filler a recognizer faithfully transcribes, stripped
/// before the leading word is examined so "can you tighten this up" is
/// classified the same as "tighten this up".
const LEADING_FILLER: &[&str] = &[
    "please", "can", "could", "would", "you", "just", "now", "ok", "okay", "so", "um", "uh", "hey",
    "maybe", "quickly", "actually",
];

/// The documented escape hatch (`docs/ux/03`): an utterance the user wants
/// written literally, even though it looks like an instruction. This is
/// what makes a false refusal cost exactly one retry.
///
/// The LEAD WORD only. The punctuation after it is deliberately not part
/// of the pattern, because the recognizer does not reliably produce a
/// colon: saying "type: tighten this up" at a live TextEdit transcribed
/// as `"Type, tighten this up."`, with a comma. Matching on `"type:"`
/// therefore never fired, and the escape hatch silently did nothing,
/// which was found by running `scripts/verify-freeform-live.sh` rather
/// than by unit test (the unit test fed the string the spec promises,
/// not the string the recognizer produces).
const DICTATION_LEAD_WORDS: &[&str] = &["type", "dictate", "literally", "insert", "quote"];

/// Longest utterance (in words) that an ambiguous verb may still be read
/// as an instruction. Spoken instructions are terse; dictated prose is
/// not bounded, so length alone separates "clean it up" from a sentence
/// that happens to start with "clean".
const MAX_INSTRUCTION_WORDS: usize = 12;

/// How many times larger the selection may be than the utterance before
/// replacing it is treated as too destructive to do on a guess.
///
/// This is the "blast radius" rule `docs/ux/03` already applies to
/// deterministic edits ("when the parse succeeds but the result would be
/// destructive and enormous, we treat scale itself as ambiguity"),
/// applied to the one case that needs it most: an utterance we are NOT
/// confident about, aimed at a lot of the user's text.
///
/// Dictating a replacement over a selection is legitimate and stays
/// supported ("just say the corrected version"): a sentence spoken over
/// a sentence is nowhere near this ratio. What the ratio catches is the
/// shape that cannot be a deliberate replacement, such as four words
/// landing on five hundred, which is far more likely to be a stale
/// select-all than an intent to delete the document.
///
/// 20 is deliberately loose. The guard exists for the catastrophic case,
/// not to second-guess ordinary editing, and every refusal it produces
/// is recoverable with the `type:` prefix.
const MAX_SELECTION_TO_UTTERANCE_RATIO: usize = 20;

/// Decide what to do with a freeform transcript spoken against `selected`.
///
/// `selected` is the text that was selected at key-down. It is used only
/// as a size reference; the classification never depends on its contents,
/// so a field that reports a selection it cannot return still behaves.
pub fn classify(transcript: &str, selected: &str) -> FreeformDisposition {
    let trimmed = transcript.trim();

    // Explicit override first, before any analysis: the user said "write
    // these words", so no heuristic gets a vote.
    if let Some(literal) = strip_dictation_prefix(trimmed) {
        return FreeformDisposition::Dictate { text: literal };
    }

    let words = words_of(trimmed);
    if words.is_empty() {
        return FreeformDisposition::Dictate {
            text: trimmed.to_string(),
        };
    }

    let significant = skip_leading_filler(&words);
    let Some(head) = significant.first() else {
        // Nothing but filler ("um, ok"): not an instruction, and inserting
        // it is what dictation does with filler anyway.
        return FreeformDisposition::Dictate {
            text: trimmed.to_string(),
        };
    };

    let refers_to_selection = refers_to_selection(&significant);
    let is_terse = words.len() <= MAX_INSTRUCTION_WORDS;

    // A meta-only verb in the leading position is decisive by itself.
    // "summarize the third paragraph" carries no pronoun and is still
    // unmistakably an instruction.
    if META_VERBS.contains(head) {
        return rewrite_request(trimmed);
    }

    // An ambiguous verb needs corroboration from both other signals.
    if AMBIGUOUS_VERBS.contains(head) && refers_to_selection && is_terse {
        return rewrite_request(trimmed);
    }

    // Nothing identified it as an instruction, so it is dictation that
    // happened while something was selected. That is the reading that
    // keeps ordinary transcription working, and inserting is what the
    // user's own keyboard would have done.
    //
    // One exception, on blast radius rather than on wording: writing
    // this would replace the selection, and if the selection dwarfs the
    // utterance that is an enormous, silent deletion made on a guess we
    // already know is uncertain. Refusing costs a retry; being wrong
    // costs the document.
    if dwarfs_the_utterance(selected, trimmed) {
        return rewrite_request(trimmed);
    }
    FreeformDisposition::Dictate {
        text: trimmed.to_string(),
    }
}

/// Is the selection so much larger than the utterance that replacing it
/// would be a deletion rather than an edit?
///
/// Compared in WORDS, not bytes, so the rule reads the same for CJK and
/// for English prose, and so a long single word cannot trip it.
fn dwarfs_the_utterance(selected: &str, transcript: &str) -> bool {
    let spoken = transcript.split_whitespace().count();
    if spoken == 0 {
        return false;
    }
    let selected_words = selected.split_whitespace().count();
    selected_words > spoken.saturating_mul(MAX_SELECTION_TO_UTTERANCE_RATIO)
}

fn rewrite_request(trimmed: &str) -> FreeformDisposition {
    FreeformDisposition::RewriteRequest {
        instruction: trimmed.to_string(),
    }
}

/// The literal text behind an explicit dictation prefix, or `None` when
/// the utterance does not carry one.
///
/// Matches a lead word followed by whatever punctuation the recognizer
/// chose (`type:`, `Type,`, `type -`, or a bare `type `), because the
/// user said a word, not a character. A lead word with nothing after it
/// is not an escape hatch: it is someone dictating the word "type".
fn strip_dictation_prefix(trimmed: &str) -> Option<String> {
    let (head, rest) = trimmed.split_once(char::is_whitespace)?;
    let lead = head
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if !DICTATION_LEAD_WORDS.contains(&lead.as_str()) {
        return None;
    }
    let mut rest = rest.trim_start();
    // Punctuation is what marks this as a prefix rather than the first
    // word of a sentence. "type the address in" is dictation and must
    // keep all its words, so a bare lead word is not an escape hatch.
    //
    // The mark may be attached ("type:") or detached ("type -"), because
    // the recognizer decides that, not the user.
    if head.len() == lead.len() {
        let (sep, after) = rest.split_once(char::is_whitespace)?;
        if sep.is_empty() || sep.chars().any(char::is_alphanumeric) {
            return None;
        }
        rest = after;
    }
    let literal = rest.trim();
    if literal.is_empty() {
        return None;
    }
    Some(literal.to_string())
}

/// Does the utterance point its verb AT the selection?
///
/// The reference has to sit in the verb's OBJECT position, not merely
/// somewhere in the sentence. Scanning the whole utterance was measured
/// (`examples/freeform_stress`) to refuse ordinary dictation that
/// happened to contain a pronoun later on:
///
/// ```text
/// "make it happen by Friday please"              -> "it" is the subject of "happen"
/// "fix that leak in the kitchen sink this weekend" -> "that" modifies "leak"
/// "edit the video and send it to Sam"            -> "it" belongs to "send"
/// "clarify with legal whether we can ship this"  -> "this" is six words downstream
/// ```
///
/// All four are dictation, and all four were refused. Requiring the
/// reference within the object window (the verb's next two words, past a
/// determiner) keeps "fix these typos" and "clean it up" while letting
/// those through.
///
/// `WINDOW` is 3 rather than 1 so "improve the phrasing here" and "make
/// this more concise" still qualify: an article or a possessive commonly
/// sits between the verb and its object.
fn refers_to_selection(significant: &[&str]) -> bool {
    const WINDOW: usize = 3;
    significant
        .iter()
        .skip(1) // the verb itself
        .take(WINDOW)
        .any(|w| SELECTION_REFERENCES.contains(w) || TEXT_PROPERTY_NOUNS.contains(w))
}

/// Lowercased words with surrounding punctuation removed. Recognizers
/// punctuate freely ("Tighten this up."), and the punctuation is never
/// part of the signal.
fn words_of(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Drop leading politeness so the verb that matters is the one examined.
/// Bounded to the first few words: a sentence made entirely of words on
/// the filler list is not an instruction.
fn skip_leading_filler(words: &[String]) -> Vec<&str> {
    let mut i = 0;
    while i < words.len() && i < 3 && LEADING_FILLER.contains(&words[i].as_str()) {
        i += 1;
    }
    words[i..].iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARAGRAPH: &str = "The customers might possibly be quite upset about this.";

    fn is_refusal(transcript: &str) -> bool {
        matches!(
            classify(transcript, PARAGRAPH),
            FreeformDisposition::RewriteRequest { .. }
        )
    }

    fn dictated(transcript: &str) -> String {
        match classify(transcript, PARAGRAPH) {
            FreeformDisposition::Dictate { text } => text,
            other => panic!("expected dictation, got {other:?}"),
        }
    }

    /// The live bug, verbatim. "tighten this up" spoken over a sentence
    /// must never be written into the document.
    #[test]
    fn the_reported_corruption_is_refused() {
        assert!(is_refusal("tighten this up"));
    }

    #[test]
    fn instructions_about_the_selection_are_refused() {
        for cmd in [
            "tighten this up",
            "make it more formal",
            "summarize this",
            "translate this to spanish",
            "make this shorter",
            "fix the grammar",
            "clean it up",
            "rephrase that",
            "can you tighten this up",
            "please summarise this for me",
            "turn this into bullet points",
            "proofread this.",
            "Tighten this up!",
        ] {
            assert!(is_refusal(cmd), "{cmd:?} must not be written over the text");
        }
    }

    /// The pronoun rule alone is not enough: "fix the grammar" carries no
    /// `this`/`it`, and writing it over the user's sentence is exactly the
    /// destructive failure. It qualifies through the text-property nouns
    /// instead, and the near-miss that must NOT qualify is pinned next to
    /// it so the two stay distinguishable.
    #[test]
    fn a_text_property_noun_counts_as_referring_to_the_selection() {
        for cmd in [
            "fix the grammar",
            "fix the spelling",
            "improve the wording",
            "adjust the tone",
        ] {
            assert!(is_refusal(cmd), "{cmd:?} is about the text, not new prose");
        }
        // Same verb, a noun from the world rather than from the text.
        assert!(!is_refusal("fix the login bug and add tests"));
        assert!(!is_refusal("fix the staging deploy before standup"));
    }

    #[test]
    fn ordinary_dictation_with_a_stale_selection_is_still_written() {
        for prose in [
            "The customers might possibly be quite upset about this.",
            "we should tell them soon",
            "fix the login bug and add tests",
            "make sure the deploy happens today",
            "turn left at the second light and park behind the building",
            "this is just a normal sentence",
            "it was a bright cold day in April and the clocks were striking thirteen",
            "meeting moved to Thursday",
        ] {
            assert!(
                !is_refusal(prose),
                "{prose:?} is dictation and must keep being written"
            );
        }
    }

    /// The regression the current behaviour was introduced to fix. A
    /// sentence that merely mentions "this" is not an instruction.
    #[test]
    fn a_long_sentence_mentioning_this_is_not_an_instruction() {
        assert!(!is_refusal(
            "make sure this gets to the team before the release goes out on Friday"
        ));
    }

    #[test]
    fn the_type_prefix_forces_literal_dictation() {
        assert_eq!(dictated("type: tighten this up"), "tighten this up");
        assert_eq!(dictated("Type: Tighten This Up"), "Tighten This Up");
        assert_eq!(dictated("dictate: summarize this"), "summarize this");
    }

    /// The punctuation the RECOGNIZER produces, not the punctuation the
    /// spec promises. Saying "type: tighten this up" at a live TextEdit
    /// transcribed as "Type, tighten this up." with a comma, so the
    /// original colon-only matching never fired and the escape hatch
    /// silently did nothing. Found by scripts/verify-freeform-live.sh.
    ///
    /// Note the expectations keep the trailing period. Only the PREFIX
    /// is removed; the rest is the user's literal text, punctuation
    /// included, because that is what "write these words" means.
    #[test]
    fn the_prefix_survives_whatever_punctuation_the_recognizer_chose() {
        for (utterance, expected) in [
            ("Type, tighten this up.", "tighten this up."),
            ("type - tighten this up", "tighten this up"),
            ("Type. tighten this up", "tighten this up"),
            ("type; tighten this up", "tighten this up"),
        ] {
            assert_eq!(
                dictated(utterance),
                expected,
                "{utterance:?} must reach the document as the literal words"
            );
        }
    }

    /// A bare prefix with nothing after it is not an escape hatch; it is
    /// the word "type" and belongs in the document.
    #[test]
    fn a_bare_prefix_is_not_an_escape_hatch() {
        assert_eq!(dictated("type:"), "type:");
    }

    /// And a lead word with NO punctuation is an ordinary sentence that
    /// happens to start with it. Stripping the first word there would
    /// silently eat a word out of the user's dictation.
    #[test]
    fn a_lead_word_without_punctuation_is_ordinary_dictation() {
        assert_eq!(
            dictated("type the address into the box"),
            "type the address into the box"
        );
        assert_eq!(
            dictated("insert the card face up"),
            "insert the card face up"
        );
    }

    #[test]
    fn dictation_preserves_the_transcript_verbatim() {
        assert_eq!(
            dictated("  we should tell them soon  "),
            "we should tell them soon"
        );
    }

    #[test]
    fn empty_and_degenerate_input_does_not_panic() {
        assert!(matches!(
            classify("", ""),
            FreeformDisposition::Dictate { .. }
        ));
        assert!(matches!(
            classify("   ", PARAGRAPH),
            FreeformDisposition::Dictate { .. }
        ));
        assert!(matches!(
            classify("...", PARAGRAPH),
            FreeformDisposition::Dictate { .. }
        ));
        assert!(matches!(
            classify("um, uh, ok", PARAGRAPH),
            FreeformDisposition::Dictate { .. }
        ));
    }

    /// Non-ASCII must not panic on a byte boundary, the bug class the
    /// workspace fuzz suite already found once in `edit-intent`.
    #[test]
    fn non_ascii_input_does_not_panic() {
        for utterance in [
            "İstanbul'a gidiyoruz",
            "ΣΊΣΥΦΟΣ",
            "型: これはテストです",
            "🎉 ship it 🎉",
            "straße",
        ] {
            let _ = classify(utterance, PARAGRAPH);
        }
    }

    /// The classification must not depend on the selection's contents, so
    /// a field that reports a selection it cannot return still behaves.
    #[test]
    fn an_empty_selection_does_not_change_the_verdict() {
        assert!(matches!(
            classify("tighten this up", ""),
            FreeformDisposition::RewriteRequest { .. }
        ));
        assert!(matches!(
            classify("we should tell them soon", ""),
            FreeformDisposition::Dictate { .. }
        ));
    }

    /// Blast radius: a handful of dictated words landing on a very large
    /// selection is a deletion, not an edit, and it would happen on a
    /// reading the classifier already knows is uncertain. `docs/ux/03`
    /// applies the same "scale is ambiguity" rule to deterministic edits.
    #[test]
    fn a_few_words_never_silently_delete_a_huge_selection() {
        let document = "word ".repeat(500);
        assert!(
            matches!(
                classify("we should tell them soon", &document),
                FreeformDisposition::RewriteRequest { .. }
            ),
            "five words replacing five hundred is a deletion made on a guess"
        );
    }

    /// And the rule must not fire on ordinary editing. "Just say the
    /// corrected version" over a sentence is the documented gesture
    /// (`docs/ux/03`) and stays supported, which is what keeps this a
    /// catastrophe guard rather than a second-guess of normal use.
    #[test]
    fn saying_the_corrected_version_over_a_sentence_still_works() {
        assert!(matches!(
            classify(
                "The customers will probably be upset about this.",
                PARAGRAPH
            ),
            FreeformDisposition::Dictate { .. }
        ));
        // Even a fairly terse correction over a full sentence.
        assert!(matches!(
            classify("they will be upset", PARAGRAPH),
            FreeformDisposition::Dictate { .. }
        ));
    }

    /// The escape hatch outranks the blast-radius guard too, so a user
    /// who really does mean to replace everything can still say so.
    #[test]
    fn the_escape_hatch_outranks_the_blast_radius_guard() {
        let document = "word ".repeat(500);
        match classify("type: start over", &document) {
            FreeformDisposition::Dictate { text } => assert_eq!(text, "start over"),
            other => panic!("the escape hatch must always write: got {other:?}"),
        }
    }
}
